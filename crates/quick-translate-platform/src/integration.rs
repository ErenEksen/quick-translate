//! Desktop integration: `.desktop` file, icon and autostart entry (design D9).

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

use quick_translate_core::APP_ID;

/// The scalable app icon installed into the hicolor theme.
pub const ICON_SVG: &str =
    include_str!("../../../packaging/icons/io.github.quicktranslate.QuickTranslate.svg");

/// Locations of the installed integration files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationPaths {
    /// `$XDG_DATA_HOME/applications/<APP_ID>.desktop`
    pub desktop_file: PathBuf,
    /// `$XDG_DATA_HOME/icons/hicolor/scalable/apps/<APP_ID>.svg`
    pub icon: PathBuf,
    /// `$XDG_CONFIG_HOME/autostart/<APP_ID>.desktop`
    pub autostart: PathBuf,
}

impl IntegrationPaths {
    pub fn new(data_home: &Path, config_home: &Path) -> Self {
        Self {
            desktop_file: data_home
                .join("applications")
                .join(format!("{APP_ID}.desktop")),
            icon: data_home
                .join("icons/hicolor/scalable/apps")
                .join(format!("{APP_ID}.svg")),
            autostart: config_home
                .join("autostart")
                .join(format!("{APP_ID}.desktop")),
        }
    }

    /// Resolves `$XDG_DATA_HOME` and `$XDG_CONFIG_HOME` with their `$HOME` defaults.
    pub fn from_env() -> io::Result<Self> {
        let data_home = xdg_dir("XDG_DATA_HOME", ".local/share")?;
        let config_home = xdg_dir("XDG_CONFIG_HOME", ".config")?;
        Ok(Self::new(&data_home, &config_home))
    }

    fn all(&self) -> [&Path; 3] {
        [&self.desktop_file, &self.icon, &self.autostart]
    }
}

fn xdg_dir(var: &str, home_relative: &str) -> io::Result<PathBuf> {
    // Relative values are invalid per the XDG Base Directory spec and must be ignored.
    if let Some(dir) = env::var_os(var).map(PathBuf::from)
        && dir.is_absolute()
    {
        return Ok(dir);
    }
    let home = env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "HOME is not set"))?;
    Ok(PathBuf::from(home).join(home_relative))
}

/// The path that `Exec` lines must point to: `$APPIMAGE` when set, else the running binary.
pub fn exec_path(
    appimage: Option<OsString>,
    current_exe: impl FnOnce() -> io::Result<PathBuf>,
) -> io::Result<PathBuf> {
    match appimage.filter(|path| !path.is_empty()) {
        Some(path) => Ok(PathBuf::from(path)),
        None => current_exe().map(unwrap_nix_wrapper),
    }
}

/// Nix's `wrapProgram` runs the real binary as `.<name>-wrapped` next to the `<name>` wrapper
/// that sets `LD_LIBRARY_PATH`. Desktop entries must point at the wrapper.
fn unwrap_nix_wrapper(exe: PathBuf) -> PathBuf {
    let wrapper = exe
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix('.')?.strip_suffix("-wrapped"))
        .map(|name| exe.with_file_name(name));
    match wrapper {
        Some(wrapper) if wrapper.is_file() => wrapper,
        _ => exe,
    }
}

/// [`exec_path`] for the current process.
pub fn exec_path_from_env() -> io::Result<PathBuf> {
    exec_path(env::var_os("APPIMAGE"), env::current_exe)
}

/// Content of the application `.desktop` file.
pub fn desktop_entry(exec: &Path) -> io::Result<String> {
    let exec = exec_arg(exec)?;
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Quick Translate\n\
         GenericName=Translator\n\
         Comment=Translate selected text with a keyboard shortcut\n\
         Exec={exec} --settings\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Utility;TextTools;\n\
         Keywords=translate;translation;translator;DeepL;\n\
         StartupNotify=false\n"
    ))
}

/// Content of the XDG autostart entry.
pub fn autostart_entry(exec: &Path) -> io::Result<String> {
    let exec = exec_arg(exec)?;
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Quick Translate\n\
         Comment=Translate selected text with a keyboard shortcut\n\
         Exec={exec} --daemon\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n"
    ))
}

/// Encodes a path as one `Exec` argument: quoted per the Desktop Entry spec when it contains a
/// reserved character, then escaped for the string value type.
fn exec_arg(path: &Path) -> io::Result<String> {
    let path = path.to_str().ok_or_else(|| {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("executable path is not valid UTF-8: {}", path.display()),
        )
    })?;
    if path.chars().any(char::is_control) {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("executable path contains control characters: {path:?}"),
        ));
    }
    const RESERVED: &[char] = &[
        ' ', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(', ')', '`',
    ];
    let mut arg = String::with_capacity(path.len() + 2);
    if path.contains(RESERVED) {
        arg.push('"');
        for c in path.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                arg.push('\\');
            }
            arg.push(c);
        }
        arg.push('"');
    } else {
        arg.push_str(path);
    }
    // `%` introduces field codes; `\` is the string-value escape character.
    Ok(arg.replace('\\', "\\\\").replace('%', "%%"))
}

/// Files touched by [`install`] or [`remove`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Changes {
    pub written: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.written.is_empty() && self.removed.is_empty()
    }
}

/// Brings the integration files up to date for `exec`. Files are only rewritten when their
/// content differs. The autostart entry is written when `autostart` is set and removed otherwise.
pub fn install(paths: &IntegrationPaths, exec: &Path, autostart: bool) -> io::Result<Changes> {
    let mut changes = Changes::default();
    if write_if_changed(&paths.desktop_file, desktop_entry(exec)?.as_bytes())? {
        changes.written.push(paths.desktop_file.clone());
    }
    if write_if_changed(&paths.icon, ICON_SVG.as_bytes())? {
        changes.written.push(paths.icon.clone());
    }
    if autostart {
        if write_if_changed(&paths.autostart, autostart_entry(exec)?.as_bytes())? {
            changes.written.push(paths.autostart.clone());
        }
    } else if remove_file(&paths.autostart)? {
        changes.removed.push(paths.autostart.clone());
    }
    Ok(changes)
}

/// Removes the `.desktop` file, icon and autostart entry. Config and secrets are not touched.
pub fn remove(paths: &IntegrationPaths) -> io::Result<Changes> {
    let mut changes = Changes::default();
    for path in paths.all() {
        if remove_file(path)? {
            changes.removed.push(path.to_path_buf());
        }
    }
    Ok(changes)
}

fn write_if_changed(path: &Path, content: &[u8]) -> io::Result<bool> {
    match fs::read(path) {
        Ok(existing) if existing == content => return Ok(false),
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(dir)?;
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(content)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map(|()| true)
}

fn remove_file(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Env {
        _dir: tempfile::TempDir,
        paths: IntegrationPaths,
        data: PathBuf,
        config: PathBuf,
    }

    fn env() -> Env {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = dir.path().join("data");
        let config = dir.path().join("config");
        let paths = IntegrationPaths::new(&data, &config);
        Env {
            _dir: dir,
            paths,
            data,
            config,
        }
    }

    fn exec_line(path: &Path) -> String {
        let content = fs::read_to_string(path).expect("read");
        content
            .lines()
            .find_map(|line| line.strip_prefix("Exec="))
            .expect("Exec line")
            .to_owned()
    }

    #[test]
    fn paths_follow_xdg_layout() {
        let env = env();
        assert_eq!(
            env.paths.desktop_file,
            env.data
                .join("applications/io.github.quicktranslate.QuickTranslate.desktop")
        );
        assert_eq!(
            env.paths.icon,
            env.data
                .join("icons/hicolor/scalable/apps/io.github.quicktranslate.QuickTranslate.svg")
        );
        assert_eq!(
            env.paths.autostart,
            env.config
                .join("autostart/io.github.quicktranslate.QuickTranslate.desktop")
        );
    }

    #[test]
    fn exec_path_prefers_appimage() {
        let path = exec_path(Some("/home/u/Apps/qt.AppImage".into()), || {
            Ok(PathBuf::from("/tmp/.mount_x/usr/bin/quick-translate"))
        })
        .expect("path");
        assert_eq!(path, PathBuf::from("/home/u/Apps/qt.AppImage"));

        let path = exec_path(None, || Ok(PathBuf::from("/usr/bin/quick-translate"))).expect("path");
        assert_eq!(path, PathBuf::from("/usr/bin/quick-translate"));

        let path =
            exec_path(Some(OsString::new()), || Ok(PathBuf::from("/usr/bin/qt"))).expect("path");
        assert_eq!(path, PathBuf::from("/usr/bin/qt"));
    }

    #[test]
    fn exec_path_uses_the_nix_wrapper() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wrapped = dir.path().join(".quick-translate-wrapped");
        let wrapper = dir.path().join("quick-translate");
        fs::write(&wrapped, "").expect("write");
        let path = exec_path(None, || Ok(wrapped.clone())).expect("path");
        assert_eq!(path, wrapped, "no wrapper next to it");

        fs::write(&wrapper, "").expect("write");
        let path = exec_path(None, || Ok(wrapped.clone())).expect("path");
        assert_eq!(path, wrapper);
    }

    #[test]
    fn first_install_writes_all_files() {
        let env = env();
        let exec = Path::new("/home/u/Apps/quick-translate.AppImage");
        let changes = install(&env.paths, exec, true).expect("install");
        assert_eq!(changes.written.len(), 3);
        assert_eq!(
            exec_line(&env.paths.desktop_file),
            "/home/u/Apps/quick-translate.AppImage --settings"
        );
        assert_eq!(
            exec_line(&env.paths.autostart),
            "/home/u/Apps/quick-translate.AppImage --daemon"
        );
        assert_eq!(fs::read_to_string(&env.paths.icon).expect("icon"), ICON_SVG);
        let desktop = fs::read_to_string(&env.paths.desktop_file).expect("desktop");
        assert!(desktop.starts_with("[Desktop Entry]\n"));
        assert!(desktop.contains("\nIcon=io.github.quicktranslate.QuickTranslate\n"));
    }

    #[test]
    fn unchanged_files_are_not_rewritten() {
        let env = env();
        let exec = Path::new("/opt/qt/quick-translate");
        install(&env.paths, exec, true).expect("install");
        let before = fs::metadata(&env.paths.desktop_file)
            .and_then(|m| m.modified())
            .expect("mtime");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let changes = install(&env.paths, exec, true).expect("reinstall");
        assert!(changes.is_empty(), "{changes:?}");
        let after = fs::metadata(&env.paths.desktop_file)
            .and_then(|m| m.modified())
            .expect("mtime");
        assert_eq!(before, after);
    }

    #[test]
    fn moved_appimage_rewrites_exec_lines() {
        let env = env();
        install(
            &env.paths,
            Path::new("/home/u/Apps/quick-translate.AppImage"),
            true,
        )
        .expect("install");
        let changes =
            install(&env.paths, Path::new("/home/u/bin/qt.AppImage"), true).expect("reinstall");
        assert_eq!(
            changes.written,
            vec![env.paths.desktop_file.clone(), env.paths.autostart.clone()]
        );
        assert_eq!(
            exec_line(&env.paths.desktop_file),
            "/home/u/bin/qt.AppImage --settings"
        );
        assert_eq!(
            exec_line(&env.paths.autostart),
            "/home/u/bin/qt.AppImage --daemon"
        );
    }

    #[test]
    fn outdated_icon_is_replaced() {
        let env = env();
        fs::create_dir_all(env.paths.icon.parent().expect("parent")).expect("mkdir");
        fs::write(&env.paths.icon, "<svg/>").expect("write");
        let changes = install(&env.paths, Path::new("/usr/bin/qt"), false).expect("install");
        assert!(changes.written.contains(&env.paths.icon));
        assert_eq!(fs::read_to_string(&env.paths.icon).expect("icon"), ICON_SVG);
    }

    #[test]
    fn autostart_disabled_removes_entry_and_keeps_it_removed() {
        let env = env();
        let exec = Path::new("/usr/bin/quick-translate");
        install(&env.paths, exec, true).expect("install");
        assert!(env.paths.autostart.exists());

        let changes = install(&env.paths, exec, false).expect("disable");
        assert_eq!(changes.removed, vec![env.paths.autostart.clone()]);
        assert!(!env.paths.autostart.exists());

        let changes = install(&env.paths, exec, false).expect("restart");
        assert!(changes.is_empty());
        assert!(!env.paths.autostart.exists());
        assert!(env.paths.desktop_file.exists());
    }

    #[test]
    fn remove_deletes_integration_but_keeps_config() {
        let env = env();
        let config_file = env.config.join("quick-translate/config.toml");
        fs::create_dir_all(config_file.parent().expect("parent")).expect("mkdir");
        fs::write(&config_file, "backend = \"deepl\"\n").expect("write");
        install(&env.paths, Path::new("/usr/bin/quick-translate"), true).expect("install");

        let changes = remove(&env.paths).expect("remove");
        assert_eq!(changes.removed.len(), 3);
        for path in env.paths.all() {
            assert!(!path.exists(), "{} still exists", path.display());
        }
        assert!(config_file.exists());

        assert!(remove(&env.paths).expect("remove again").is_empty());
    }

    #[test]
    fn exec_paths_with_spaces_are_quoted() {
        let entry =
            desktop_entry(Path::new("/home/u/My Apps/Quick Translate.AppImage")).expect("entry");
        assert!(
            entry.contains("\nExec=\"/home/u/My Apps/Quick Translate.AppImage\" --settings\n"),
            "{entry}"
        );
    }

    #[test]
    fn exec_special_characters_are_escaped() {
        assert_eq!(
            exec_arg(Path::new("/opt/qt-1.0/quick_translate")).expect("arg"),
            "/opt/qt-1.0/quick_translate"
        );
        // `$` and `"` are backslash-escaped inside quotes, then `\` is doubled for the string type.
        assert_eq!(
            exec_arg(Path::new("/a b/$x\"y")).expect("arg"),
            r#""/a b/\\$x\\"y""#
        );
        assert_eq!(exec_arg(Path::new(r"/a\b")).expect("arg"), r#""/a\\\\b""#);
        assert_eq!(
            exec_arg(Path::new("/x/100%/qt")).expect("arg"),
            "/x/100%%/qt"
        );
        assert!(exec_arg(Path::new("/x/a\nb")).is_err());
    }

    #[test]
    fn icon_is_a_scalable_svg() {
        assert!(ICON_SVG.trim_start().starts_with("<svg"));
        assert!(ICON_SVG.contains("viewBox=\"0 0 128 128\""));
    }
}
