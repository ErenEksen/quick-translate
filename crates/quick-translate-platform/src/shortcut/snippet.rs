//! Ready-to-paste compositor bindings that run `<exe> --toggle` (design D6).
//!
//! The application only shows these. It never writes compositor or desktop config files.

use std::{
    ffi::{OsStr, OsString},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use quick_translate_core::Compositor;

/// Binary name used when the executable is on `PATH`.
pub const BIN_NAME: &str = "quick-translate";

/// The program a binding must run, plus `--toggle`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToggleCommand {
    program: String,
}

impl ToggleCommand {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// `$APPIMAGE` when running from an AppImage, else `quick-translate` when it is on
    /// `PATH`, else the absolute path of the running executable.
    pub fn detect() -> Self {
        Self::detect_from(
            std::env::var_os("APPIMAGE"),
            std::env::var_os("PATH"),
            std::env::current_exe().ok(),
        )
    }

    /// [`detect`](Self::detect) with injected inputs.
    pub fn detect_from(
        appimage: Option<OsString>,
        path: Option<OsString>,
        current_exe: Option<PathBuf>,
    ) -> Self {
        if let Some(appimage) = appimage.filter(|a| !a.is_empty()) {
            return Self::new(appimage.to_string_lossy());
        }
        if path.is_some_and(|path| on_path(&path, BIN_NAME)) {
            return Self::new(BIN_NAME);
        }
        current_exe.map_or_else(
            || Self::new(BIN_NAME),
            |exe| Self::new(exe.to_string_lossy()),
        )
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    /// Argument vector: program, then `--toggle`.
    pub fn argv(&self) -> [&str; 2] {
        [&self.program, "--toggle"]
    }

    /// The command as one POSIX shell command line.
    pub fn shell(&self) -> String {
        format!("{} --toggle", shell_quote(&self.program))
    }
}

fn on_path(path: &OsStr, name: &str) -> bool {
    std::env::split_paths(path).any(|dir| is_executable(&dir.join(name)))
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Snippet flavor, one per supported environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnippetFormat {
    Sway,
    Hyprland,
    Niri,
    River,
    Labwc,
    Wayfire,
    Cosmic,
    Kde,
    Gnome,
    Generic,
}

impl SnippetFormat {
    pub const ALL: [Self; 10] = [
        Self::Sway,
        Self::Hyprland,
        Self::Niri,
        Self::River,
        Self::Labwc,
        Self::Wayfire,
        Self::Cosmic,
        Self::Kde,
        Self::Gnome,
        Self::Generic,
    ];

    pub fn for_compositor(compositor: &Compositor) -> Self {
        match compositor {
            Compositor::Sway => Self::Sway,
            Compositor::Hyprland => Self::Hyprland,
            Compositor::Niri => Self::Niri,
            Compositor::River => Self::River,
            Compositor::Labwc => Self::Labwc,
            Compositor::Wayfire => Self::Wayfire,
            Compositor::Cosmic => Self::Cosmic,
            Compositor::Kde => Self::Kde,
            Compositor::Gnome => Self::Gnome,
            Compositor::Other(_) | Compositor::Unknown => Self::Generic,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Sway => "Sway",
            Self::Hyprland => "Hyprland",
            Self::Niri => "niri",
            Self::River => "river",
            Self::Labwc => "labwc",
            Self::Wayfire => "Wayfire",
            Self::Cosmic => "COSMIC",
            Self::Kde => "KDE Plasma",
            Self::Gnome => "GNOME",
            Self::Generic => "Other environments",
        }
    }

    /// The file the snippet belongs in, if it is a config-file snippet.
    pub fn config_file(self) -> Option<&'static str> {
        match self {
            Self::Sway => Some("~/.config/sway/config"),
            Self::Hyprland => Some("~/.config/hypr/hyprland.conf"),
            Self::Niri => Some("~/.config/niri/config.kdl"),
            Self::River => Some("~/.config/river/init"),
            Self::Labwc => Some("~/.config/labwc/rc.xml"),
            Self::Wayfire => Some("~/.config/wayfire.ini"),
            Self::Cosmic | Self::Kde | Self::Gnome | Self::Generic => None,
        }
    }
}

/// A binding snippet shown in settings with a Copy button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub format: SnippetFormat,
    /// Where and how to apply [`text`](Self::text).
    pub instructions: String,
    /// The text the Copy button places on the clipboard.
    pub text: String,
}

impl Snippet {
    /// Instructions and text as one block, for plain-text display.
    pub fn render(&self) -> String {
        format!(
            "# {}\n{}\n\n{}\n",
            self.format.name(),
            self.instructions,
            self.text
        )
    }
}

/// The snippet for the detected compositor.
pub fn snippet_for(compositor: &Compositor, command: &ToggleCommand) -> Snippet {
    generate(SnippetFormat::for_compositor(compositor), command)
}

pub fn generate(format: SnippetFormat, command: &ToggleCommand) -> Snippet {
    let shell = command.shell();
    let (instructions, text) = match format {
        SnippetFormat::Sway => (
            "Add this line to ~/.config/sway/config, then run `swaymsg reload`.".to_owned(),
            format!("bindsym Ctrl+Alt+c exec {shell}"),
        ),
        SnippetFormat::Hyprland => (
            "Add this line to ~/.config/hypr/hyprland.conf. Hyprland reloads it automatically."
                .to_owned(),
            format!("bind = CTRL ALT, C, exec, {}", shell.replace('#', "##")),
        ),
        SnippetFormat::Niri => (
            "Add this line inside the `binds { }` section of ~/.config/niri/config.kdl. \
             niri reloads it automatically."
                .to_owned(),
            format!(
                "Ctrl+Alt+C {{ spawn {} {}; }}",
                kdl_string(command.program()),
                kdl_string("--toggle")
            ),
        ),
        SnippetFormat::River => (
            "Add this line to your river init script (~/.config/river/init), then run it \
             once or restart river. This uses `riverctl` (river 0.3 and river-classic)."
                .to_owned(),
            format!(
                "riverctl map normal Control+Alt C spawn {}",
                shell_quote_command(&shell)
            ),
        ),
        SnippetFormat::Labwc => (
            "Add this inside the `<keyboard>` section of ~/.config/labwc/rc.xml, then run \
             `labwc --reconfigure`."
                .to_owned(),
            format!(
                "<keybind key=\"C-A-c\">\n  <action name=\"Execute\" command=\"{}\" />\n</keybind>",
                xml_attr(&shell)
            ),
        ),
        SnippetFormat::Wayfire => (
            "Add these lines to the `[command]` section of ~/.config/wayfire.ini (create the \
             section if it is missing, and make sure `command` is in `plugins` under \
             `[core]`). Wayfire reloads it automatically."
                .to_owned(),
            format!(
                "binding_quick_translate = <ctrl> <alt> KEY_C\ncommand_quick_translate = {shell}"
            ),
        ),
        SnippetFormat::Cosmic => (
            "Open Settings > Keyboard > Keyboard Shortcuts > Custom Shortcuts > Add Shortcut. \
             Name it \"Quick Translate\", paste this command, and press Ctrl+Alt+C."
                .to_owned(),
            shell,
        ),
        SnippetFormat::Kde => (
            "Open System Settings > Keyboard > Shortcuts > Add New > Command or Script. \
             Paste this command, then set Ctrl+Alt+C as its shortcut."
                .to_owned(),
            shell,
        ),
        SnippetFormat::Gnome => (
            "Open Settings > Keyboard > View and Customize Shortcuts > Custom Shortcuts > \
             Add Shortcut. Name it \"Quick Translate\", paste this command, and set Ctrl+Alt+C."
                .to_owned(),
            shell,
        ),
        SnippetFormat::Generic => (
            "Bind Ctrl+Alt+C (or any key you like) in your compositor's or desktop's \
             shortcut settings to run this command."
                .to_owned(),
            shell,
        ),
    };
    Snippet {
        format,
        instructions,
        text,
    }
}

/// POSIX shell single-word quoting. Plain words are left as they are.
fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b));
    if plain {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Quote a whole command line as one shell word, keeping it readable.
fn shell_quote_command(s: &str) -> String {
    if s.contains('\'') {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            if matches!(c, '"' | '\\' | '$' | '`') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    } else {
        format!("'{s}'")
    }
}

fn kdl_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn xml_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quoting() {
        assert_eq!(
            shell_quote("/usr/bin/quick-translate"),
            "/usr/bin/quick-translate"
        );
        assert_eq!(shell_quote("/a b/c"), "'/a b/c'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote_command("a --toggle"), "'a --toggle'");
        assert_eq!(
            shell_quote_command(r"'a b'\''c' --toggle"),
            r#""'a b'\\''c' --toggle""#
        );
    }

    #[test]
    fn appimage_wins() {
        let cmd = ToggleCommand::detect_from(
            Some("/home/u/Apps/QT.AppImage".into()),
            Some("/usr/bin".into()),
            Some("/tmp/.mount_x/usr/bin/quick-translate".into()),
        );
        assert_eq!(cmd.program(), "/home/u/Apps/QT.AppImage");
    }

    #[test]
    fn path_then_current_exe() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join(BIN_NAME);
        std::fs::write(&bin, "").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let exe = Some(PathBuf::from("/opt/qt/quick-translate"));

        let on_path = ToggleCommand::detect_from(None, Some(dir.path().into()), exe.clone());
        assert_eq!(on_path.program(), BIN_NAME);

        let empty = tempfile::tempdir().unwrap();
        let off_path = ToggleCommand::detect_from(Some("".into()), Some(empty.path().into()), exe);
        assert_eq!(off_path.program(), "/opt/qt/quick-translate");

        assert_eq!(
            ToggleCommand::detect_from(None, None, None).program(),
            BIN_NAME
        );
    }

    #[test]
    fn non_executable_is_not_on_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(BIN_NAME), "").unwrap();
        let cmd = ToggleCommand::detect_from(None, Some(dir.path().into()), Some("/x/qt".into()));
        assert_eq!(cmd.program(), "/x/qt");
    }
}
