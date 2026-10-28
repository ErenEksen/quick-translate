//! Non-secret settings stored in `$XDG_CONFIG_HOME/quick-translate/config.toml` (design D8).
//!
//! Secrets are never part of this model. Writes are atomic and preserve unknown keys.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, TableLike};

use crate::language;

/// The active translation backend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    #[default]
    Deepl,
    Openai,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub backend: BackendKind,
    /// Target language code (DeepL target code such as `EN-US`, `TR`). `None` means derive from locale.
    pub target_lang: Option<String>,
    pub deepl: DeeplConfig,
    pub openai: OpenAiConfig,
    pub popup: PopupConfig,
    pub autostart: bool,
    /// Opt-in evdev shortcut listener (off by default).
    pub evdev_enabled: bool,
    /// Total request timeout in seconds (default 30).
    pub request_timeout_secs: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct DeeplConfig {
    /// Host that last worked for the stored key, e.g. `api-free.deepl.com`.
    pub cached_host: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct OpenAiConfig {
    pub base_url: String,
    pub model: String,
    /// The user confirmed a plain-http non-loopback `base_url`.
    pub allow_insecure: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PopupConfig {
    /// Logical pixels.
    pub width: f32,
    pub height: f32,
    /// Fraction of the width given to the source pane, 0.0..=1.0.
    pub splitter: f32,
}

/// Default OpenAI-compatible endpoint.
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
/// Default OpenAI-compatible model.
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";
/// Default total request timeout in seconds.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: BackendKind::Deepl,
            target_lang: None,
            deepl: DeeplConfig::default(),
            openai: OpenAiConfig::default(),
            popup: PopupConfig::default(),
            autostart: true,
            evdev_enabled: false,
            request_timeout_secs: DEFAULT_REQUEST_TIMEOUT_SECS,
        }
    }
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_OPENAI_BASE_URL.to_owned(),
            model: DEFAULT_OPENAI_MODEL.to_owned(),
            allow_insecure: false,
        }
    }
}

impl Default for PopupConfig {
    fn default() -> Self {
        Self {
            width: 720.0,
            height: 320.0,
            splitter: 0.5,
        }
    }
}

impl Config {
    /// The target language to use: the stored choice if it is a known DeepL target (a source
    /// code such as `EN` is mapped to its target, `EN-US`), otherwise the locale default.
    pub fn effective_target_lang(&self) -> &'static str {
        self.effective_target_lang_from(|key| std::env::var(key).ok())
    }

    /// Testable variant of [`Config::effective_target_lang`] with an injected environment lookup.
    pub fn effective_target_lang_from(&self, env: impl Fn(&str) -> Option<String>) -> &'static str {
        self.target_lang
            .as_deref()
            .and_then(|code| {
                language::target_by_code(code)
                    .map(|l| l.code)
                    .or_else(|| language::source_to_target(code))
            })
            .unwrap_or_else(|| language::default_target_from(env))
    }
}

/// Result of loading the config file.
#[derive(Debug)]
pub enum LoadOutcome {
    /// The file existed and parsed.
    Loaded(Config),
    /// No file: defaults.
    Missing(Config),
    /// The file is malformed: defaults are used and the file is left untouched until the user saves.
    Malformed {
        config: Config,
        path: PathBuf,
        error: String,
    },
}

impl LoadOutcome {
    pub fn config(&self) -> &Config {
        match self {
            LoadOutcome::Loaded(c) | LoadOutcome::Missing(c) => c,
            LoadOutcome::Malformed { config, .. } => config,
        }
    }
}

/// Loads and saves the config file, keeping the original TOML document so unknown keys survive.
#[derive(Debug)]
pub struct ConfigStore {
    path: PathBuf,
    /// The document from the last successful load or save. `None` before a load, after a
    /// missing or malformed file; a save then writes a fresh document.
    doc: Option<DocumentMut>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot write {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot serialize config: {0}")]
    Serialize(String),
}

impl ConfigStore {
    /// `$XDG_CONFIG_HOME/quick-translate/config.toml`, falling back to `~/.config/...`.
    pub fn default_path() -> PathBuf {
        Self::default_path_from(|key| std::env::var_os(key).map(PathBuf::from))
    }

    /// Testable variant of [`ConfigStore::default_path`]. A relative `XDG_CONFIG_HOME` is
    /// ignored, as the XDG Base Directory spec requires.
    pub fn default_path_from(env: impl Fn(&str) -> Option<PathBuf>) -> PathBuf {
        let base = env("XDG_CONFIG_HOME")
            .filter(|p| p.is_absolute())
            .or_else(|| {
                env("HOME")
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(|home| home.join(".config"))
            })
            .unwrap_or_else(|| PathBuf::from(".config"));
        base.join("quick-translate").join("config.toml")
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            doc: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Never fails: a missing or malformed file yields defaults.
    pub fn load(&mut self) -> LoadOutcome {
        self.doc = None;
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return LoadOutcome::Missing(Config::default());
            }
            Err(e) => return self.malformed(e.to_string()),
        };
        let doc = match text.parse::<DocumentMut>() {
            Ok(doc) => doc,
            Err(e) => return self.malformed(e.to_string()),
        };
        match toml::from_str::<Config>(&text) {
            Ok(config) => {
                self.doc = Some(doc);
                LoadOutcome::Loaded(config)
            }
            Err(e) => self.malformed(e.to_string()),
        }
    }

    fn malformed(&self, error: String) -> LoadOutcome {
        let error = error.trim_end().to_owned();
        tracing::warn!(path = %self.path.display(), %error, "malformed config, using defaults");
        LoadOutcome::Malformed {
            config: Config::default(),
            path: self.path.clone(),
            error,
        }
    }

    /// Atomic write (temp file in the same directory, then rename). Creates the directory.
    /// Unknown keys from the last load are preserved.
    pub fn save(&mut self, config: &Config) -> Result<(), ConfigError> {
        let fresh: DocumentMut = toml::to_string(config)
            .map_err(|e| ConfigError::Serialize(e.to_string()))?
            .parse()
            .map_err(|e: toml_edit::TomlError| ConfigError::Serialize(e.to_string()))?;
        let mut doc = self.doc.clone().unwrap_or_default();
        merge_known(doc.as_table_mut(), fresh.as_table(), schema().as_table());
        let text = doc.to_string();
        atomic_write(&self.path, text.as_bytes()).map_err(|source| ConfigError::Io {
            path: self.path.clone(),
            source,
        })?;
        self.doc = Some(doc);
        Ok(())
    }
}

/// Every key the model knows about, with all optional fields present.
fn schema() -> DocumentMut {
    let mut full = Config {
        target_lang: Some(String::new()),
        ..Config::default()
    };
    full.deepl.cached_host = Some(String::new());
    toml::to_string(&full)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_default()
}

/// Update `doc` with the known keys from `fresh`, removing known keys absent from `fresh`
/// (unset options) and leaving every unknown key, comment and ordering in place.
fn merge_known(doc: &mut dyn TableLike, fresh: &dyn TableLike, schema: &dyn TableLike) {
    for (key, schema_item) in schema.iter() {
        let Some(new_item) = fresh.get(key) else {
            doc.remove(key);
            continue;
        };
        match (doc.get_mut(key), new_item) {
            (Some(old), new) if schema_item.is_table_like() && old.is_table_like() => {
                if let (Some(old), Some(new), Some(schema)) = (
                    old.as_table_like_mut(),
                    new.as_table_like(),
                    schema_item.as_table_like(),
                ) {
                    merge_known(old, new, schema);
                }
            }
            (Some(Item::Value(old)), Item::Value(new)) => {
                let decor = old.decor().clone();
                *old = new.clone();
                *old.decor_mut() = decor;
            }
            (Some(old), new) => *old = new.clone(),
            (None, new) => {
                doc.insert(key, new.clone());
            }
        }
    }
}

static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Write to a temp file in the same directory, fsync, then rename over `path`.
fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?
        .to_string_lossy();
    let tmp = dir.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)?;
        // Persist the rename itself; not every filesystem supports fsync on directories.
        if let Ok(dir) = File::open(dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &tempfile::TempDir) -> ConfigStore {
        ConfigStore::new(dir.path().join("quick-translate").join("config.toml"))
    }

    fn sample() -> Config {
        Config {
            backend: BackendKind::Openai,
            target_lang: Some("DE".into()),
            deepl: DeeplConfig {
                cached_host: Some("api-free.deepl.com".into()),
            },
            openai: OpenAiConfig {
                base_url: "http://localhost:11434/v1".into(),
                model: "llama3".into(),
                allow_insecure: true,
            },
            popup: PopupConfig {
                width: 800.0,
                height: 400.0,
                splitter: 0.3,
            },
            autostart: false,
            evdev_enabled: true,
            request_timeout_secs: 45,
        }
    }

    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.backend, BackendKind::Deepl);
        assert_eq!(c.target_lang, None);
        assert!(c.autostart);
        assert!(!c.evdev_enabled);
        assert_eq!(c.request_timeout_secs, 30);
        assert_eq!(
            (c.popup.width, c.popup.height, c.popup.splitter),
            (720.0, 320.0, 0.5)
        );
        assert_eq!(c.openai.base_url, "https://api.openai.com/v1");
        assert!(!c.openai.model.is_empty());
        assert!(!c.openai.allow_insecure);
        assert_eq!(c.deepl.cached_host, None);
    }

    #[test]
    fn default_path_prefers_absolute_xdg_config_home() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                vars.iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| PathBuf::from(v))
            }
        };
        assert_eq!(
            ConfigStore::default_path_from(env(&[
                ("XDG_CONFIG_HOME", "/xdg"),
                ("HOME", "/home/u")
            ])),
            Path::new("/xdg/quick-translate/config.toml")
        );
        assert_eq!(
            ConfigStore::default_path_from(env(&[("HOME", "/home/u")])),
            Path::new("/home/u/.config/quick-translate/config.toml")
        );
        assert_eq!(
            ConfigStore::default_path_from(env(&[("XDG_CONFIG_HOME", "rel"), ("HOME", "/home/u")])),
            Path::new("/home/u/.config/quick-translate/config.toml")
        );
    }

    #[test]
    fn missing_file_and_directory_yield_defaults_and_save_creates_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = store(&dir);
        assert!(!store.path().parent().expect("parent").exists());
        let outcome = store.load();
        assert!(matches!(outcome, LoadOutcome::Missing(_)));
        assert_eq!(outcome.config(), &Config::default());

        store.save(&Config::default()).expect("save");
        assert!(store.path().is_file());
        let mut reloaded = ConfigStore::new(store.path());
        assert!(matches!(reloaded.load(), LoadOutcome::Loaded(c) if c == Config::default()));
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = store(&dir);
        store.load();
        let config = sample();
        store.save(&config).expect("save");

        let mut reloaded = ConfigStore::new(store.path());
        match reloaded.load() {
            LoadOutcome::Loaded(c) => assert_eq!(c, config),
            other => panic!("unexpected {other:?}"),
        }

        let text = fs::read_to_string(store.path()).expect("read");
        assert!(text.contains("backend = \"openai\""), "{text}");
        assert!(text.contains("target-lang = \"DE\""), "{text}");
        assert!(text.contains("splitter = 0.3\n"), "{text}");

        // Clearing optional fields removes their keys.
        let mut cleared = config.clone();
        cleared.target_lang = None;
        cleared.deepl.cached_host = None;
        reloaded.save(&cleared).expect("save");
        let text = fs::read_to_string(store.path()).expect("read");
        assert!(!text.contains("target-lang"), "{text}");
        assert!(!text.contains("cached-host"), "{text}");
        let mut again = ConfigStore::new(store.path());
        assert!(matches!(again.load(), LoadOutcome::Loaded(c) if c == cleared));
    }

    #[test]
    fn unknown_keys_and_comments_survive_save() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = store(&dir);
        fs::create_dir_all(store.path().parent().expect("parent")).expect("mkdir");
        let original = "\
# my settings
backend = \"deepl\" # keep this comment
future-flag = true
target-lang = \"TR\"

[popup]
width = 640.0
opacity = 0.9

[deepl]
cached-host = \"api.deepl.com\"

[experimental]
thing = \"x\"
list = [1, 2, 3]
";
        fs::write(store.path(), original).expect("write");
        let mut config = match store.load() {
            LoadOutcome::Loaded(c) => c,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(config.target_lang.as_deref(), Some("TR"));
        assert_eq!(config.popup.width, 640.0);
        assert_eq!(config.popup.height, 320.0);

        config.backend = BackendKind::Openai;
        config.popup.width = 700.0;
        config.deepl.cached_host = None;
        store.save(&config).expect("save");

        let text = fs::read_to_string(store.path()).expect("read");
        assert!(text.starts_with("# my settings\n"), "{text}");
        assert!(
            text.contains("backend = \"openai\" # keep this comment"),
            "{text}"
        );
        assert!(text.contains("future-flag = true"), "{text}");
        assert!(text.contains("opacity = 0.9"), "{text}");
        assert!(
            text.contains("[experimental]\nthing = \"x\"\nlist = [1, 2, 3]"),
            "{text}"
        );
        assert!(!text.contains("cached-host"), "{text}");

        let doc: DocumentMut = text.parse().expect("valid toml");
        assert_eq!(doc["popup"]["width"].as_float(), Some(700.0));
        assert_eq!(doc["autostart"].as_bool(), Some(true));
        let mut reloaded = ConfigStore::new(store.path());
        assert!(matches!(reloaded.load(), LoadOutcome::Loaded(c) if c == config));
    }

    #[test]
    fn malformed_file_yields_defaults_and_is_left_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = store(&dir);
        fs::create_dir_all(store.path().parent().expect("parent")).expect("mkdir");
        let broken = "backend = \"deepl\"\n[popup\nwidth = \n";
        fs::write(store.path(), broken).expect("write");

        match store.load() {
            LoadOutcome::Malformed {
                config,
                path,
                error,
            } => {
                assert_eq!(config, Config::default());
                assert_eq!(path, store.path());
                assert!(!error.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(fs::read_to_string(store.path()).expect("read"), broken);

        // Saving after the user confirms settings replaces the broken file.
        store.save(&Config::default()).expect("save");
        let mut reloaded = ConfigStore::new(store.path());
        assert!(matches!(reloaded.load(), LoadOutcome::Loaded(c) if c == Config::default()));
    }

    #[test]
    fn wrong_types_are_malformed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = store(&dir);
        fs::create_dir_all(store.path().parent().expect("parent")).expect("mkdir");
        fs::write(store.path(), "autostart = \"yes\"\n").expect("write");
        assert!(matches!(store.load(), LoadOutcome::Malformed { .. }));
        fs::write(store.path(), "backend = \"google\"\n").expect("write");
        assert!(matches!(store.load(), LoadOutcome::Malformed { .. }));
    }

    #[test]
    fn save_leaves_no_temp_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = store(&dir);
        store.load();
        store.save(&sample()).expect("save");
        store.save(&Config::default()).expect("save again");
        let entries: Vec<_> = fs::read_dir(store.path().parent().expect("parent"))
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("config.toml")]);
    }

    #[test]
    fn save_reports_io_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blocker = dir.path().join("file");
        fs::write(&blocker, "").expect("write");
        let mut store = ConfigStore::new(blocker.join("config.toml"));
        assert!(matches!(
            store.save(&Config::default()),
            Err(ConfigError::Io { .. })
        ));
    }

    #[test]
    fn effective_target_lang() {
        let env = |key: &str| (key == "LANG").then(|| "tr_TR.UTF-8".to_owned());
        let mut c = Config::default();
        assert_eq!(c.effective_target_lang_from(env), "TR");
        c.target_lang = Some("de".into());
        assert_eq!(c.effective_target_lang_from(env), "DE");
        c.target_lang = Some("EN".into());
        assert_eq!(c.effective_target_lang_from(env), "EN-US");
        c.target_lang = Some("bogus".into());
        assert_eq!(c.effective_target_lang_from(env), "TR");
    }

    #[test]
    fn serialized_config_has_no_secret_fields() {
        let text = toml::to_string(&sample()).expect("serialize");
        for word in ["key", "token", "secret", "password"] {
            assert!(
                !text.to_ascii_lowercase().contains(word),
                "{word} in {text}"
            );
        }
    }
}
