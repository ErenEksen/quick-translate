//! Configuration, languages, capabilities and shared types.

pub mod capabilities;
pub mod config;
pub mod language;

pub use capabilities::{Capabilities, Compositor, DataControl};
pub use config::{BackendKind, Config, ConfigError, ConfigStore, LoadOutcome};
pub use language::Language;

/// Application ID used for the D-Bus name, `.desktop` file, portal registry and keyring attributes.
pub const APP_ID: &str = "io.github.quicktranslate.QuickTranslate";

/// D-Bus object path exported by the daemon.
pub const DBUS_PATH: &str = "/io/github/quicktranslate/QuickTranslate";

/// Maximum number of characters kept from a captured selection.
pub const MAX_SOURCE_CHARS: usize = 10_000;
