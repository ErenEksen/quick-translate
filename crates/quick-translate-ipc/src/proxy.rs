//! Async client proxy, for callers that already run Tokio (e.g. the settings process).

use std::collections::HashMap;

use zbus::proxy;
use zbus::zvariant::{OwnedValue, Value};

#[proxy(
    interface = "io.github.quicktranslate.QuickTranslate",
    default_service = "io.github.quicktranslate.QuickTranslate",
    default_path = "/io/github/quicktranslate/QuickTranslate"
)]
pub trait QuickTranslate {
    fn toggle(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;
    fn show(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;
    fn settings(&self) -> zbus::Result<()>;
    fn quit(&self) -> zbus::Result<()>;
    fn status(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    fn reload_config(&self) -> zbus::Result<()>;
    /// `backend` is `deepl` or `openai`; an empty `key` drops the session key.
    fn set_session_secret(&self, backend: &str, key: &str) -> zbus::Result<()>;
    fn shortcut_info(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
    fn configure_shortcut(&self, activation_token: &str) -> zbus::Result<()>;
    fn set_evdev_enabled(&self, enabled: bool) -> zbus::Result<()>;

    #[zbus(signal)]
    fn shortcut_changed(&self, trigger: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    fn settings_requested(&self) -> zbus::Result<()>;
}

/// The human-readable part of a failed call: the daemon's error message when it sent one.
pub fn error_message(err: &zbus::Error) -> String {
    match err {
        zbus::Error::MethodError(_, Some(message), _) => message.clone(),
        zbus::Error::FDO(err) => match err.as_ref() {
            zbus::fdo::Error::Failed(message) => message.clone(),
            other => other.to_string(),
        },
        other => other.to_string(),
    }
}
