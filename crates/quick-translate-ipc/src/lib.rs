//! IPC between the quick-translate CLI and the resident daemon (design D1).
//!
//! The daemon owns `io.github.quicktranslate.QuickTranslate` on the session bus and serves
//! [`server::QuickTranslateInterface`] at [`OBJECT_PATH`]; every call becomes a
//! [`DaemonCommand`] on a channel. Without a session bus it listens on the Unix socket from
//! [`socket::default_path`] instead.
//!
//! The CLI forwards with [`client::forward`], which is fully blocking and never starts a Tokio
//! runtime. The settings process uses the async [`proxy::QuickTranslateProxy`], which also
//! carries the settings-only methods (`ReloadConfig`, `SetSessionSecret`, `ShortcutInfo`,
//! `ConfigureShortcut`, `SetEvdevEnabled`) and the `ShortcutChanged`/`SettingsRequested` signals.

use std::collections::HashMap;

use tokio::sync::oneshot;
use zbus::zvariant::{OwnedValue, Value};

pub mod client;
pub mod proxy;
pub mod server;
pub mod settings;
pub mod socket;
#[doc(hidden)]
pub mod testing;
mod wire;

pub use client::{Endpoints, ForwardError, Reply, forward};
pub use server::{
    BusClaim, QuickTranslateInterface, claim_bus_name, emit_settings_requested,
    emit_shortcut_changed,
};
pub use settings::{SecretString, ShortcutInfo, ShortcutMechanism, SnippetInfo};

/// Well-known bus name owned by the daemon.
pub const BUS_NAME: &str = quick_translate_core::APP_ID;
/// Interface name of the daemon object.
pub const INTERFACE: &str = quick_translate_core::APP_ID;
/// Object path exported by the daemon.
pub const OBJECT_PATH: &str = quick_translate_core::DBUS_PATH;
/// Options key carrying the `XDG_ACTIVATION_TOKEN` of the invoking process.
pub const ACTIVATION_TOKEN_KEY: &str = "activation-token";

/// A command delivered to the daemon from D-Bus or the socket fallback.
#[derive(Debug)]
pub enum DaemonCommand {
    Toggle {
        options: Options,
    },
    Show {
        options: Options,
    },
    Settings,
    Quit,
    Status {
        reply: oneshot::Sender<DaemonStatus>,
    },
    /// Settings saved `config.toml`: re-read it and apply backend, target, timeout, evdev and
    /// autostart (re-run the desktop integration install).
    ReloadConfig {
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Keep an API key in daemon memory for this session only, shadowing the keyring. An
    /// empty key drops the session key (the keyring is not touched).
    SetSessionSecret {
        backend: quick_translate_core::BackendKind,
        key: SecretString,
    },
    ShortcutInfo {
        reply: oneshot::Sender<ShortcutInfo>,
    },
    /// "Change shortcut": open the portal's configuration UI (GlobalShortcuts v2+). Reply once
    /// the portal accepted the request, without waiting for its dialog to close.
    ConfigureShortcut {
        activation_token: Option<String>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Start or stop the input-device fallback. Enabling replies with the error (e.g. missing
    /// permission) when the listener cannot start, leaving the mode inactive.
    SetEvdevEnabled {
        enabled: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

/// Options passed with `Toggle` and `Show` (the `a{sv}` argument).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    pub activation_token: Option<String>,
}

impl Options {
    /// Options for the current process: the activation token comes from `XDG_ACTIVATION_TOKEN`.
    pub fn from_env() -> Self {
        Self {
            activation_token: std::env::var("XDG_ACTIVATION_TOKEN")
                .ok()
                .filter(|token| !token.is_empty()),
        }
    }

    pub fn to_dict(&self) -> HashMap<&'static str, Value<'_>> {
        let mut dict = HashMap::new();
        if let Some(token) = &self.activation_token {
            dict.insert(ACTIVATION_TOKEN_KEY, Value::from(token.as_str()));
        }
        dict
    }

    /// Reads known keys and ignores unknown ones, so newer clients keep working.
    pub fn from_dict(dict: &HashMap<String, OwnedValue>) -> Self {
        Self {
            activation_token: dict
                .get(ACTIVATION_TOKEN_KEY)
                .and_then(|value| <&str>::try_from(value).ok())
                .filter(|token| !token.is_empty())
                .map(str::to_owned),
        }
    }
}

/// Reply of `Status()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DaemonStatus {
    pub version: String,
    pub pid: u32,
    /// Human-readable capability report (the `Display` of the capability probe).
    pub capabilities: String,
    pub popup_visible: bool,
}

impl DaemonStatus {
    pub const VERSION: &'static str = "version";
    pub const PID: &'static str = "pid";
    pub const CAPABILITIES: &'static str = "capabilities";
    pub const POPUP_VISIBLE: &'static str = "popup-visible";

    pub fn to_dict(&self) -> HashMap<String, OwnedValue> {
        HashMap::from([
            (
                Self::VERSION.into(),
                OwnedValue::from(owned_str(&self.version)),
            ),
            (Self::PID.into(), OwnedValue::from(self.pid)),
            (
                Self::CAPABILITIES.into(),
                OwnedValue::from(owned_str(&self.capabilities)),
            ),
            (
                Self::POPUP_VISIBLE.into(),
                OwnedValue::from(self.popup_visible),
            ),
        ])
    }

    pub fn from_dict(dict: &HashMap<String, OwnedValue>) -> Self {
        let text = |key: &str| {
            dict.get(key)
                .and_then(|value| <&str>::try_from(value).ok())
                .unwrap_or_default()
                .to_owned()
        };
        Self {
            version: text(Self::VERSION),
            pid: dict
                .get(Self::PID)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or_default(),
            capabilities: text(Self::CAPABILITIES),
            popup_visible: dict
                .get(Self::POPUP_VISIBLE)
                .and_then(|value| bool::try_from(value).ok())
                .unwrap_or_default(),
        }
    }
}

pub(crate) fn owned_str(text: &str) -> zbus::zvariant::Str<'static> {
    zbus::zvariant::Str::from(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(dict: HashMap<&'static str, Value<'_>>) -> HashMap<String, OwnedValue> {
        dict.into_iter()
            .map(|(key, value)| (key.to_owned(), value.try_to_owned().expect("owned")))
            .collect()
    }

    #[test]
    fn options_round_trip() {
        let options = Options {
            activation_token: Some("kwin-42".into()),
        };
        assert_eq!(Options::from_dict(&owned(options.to_dict())), options);
        assert_eq!(
            Options::from_dict(&owned(Options::default().to_dict())),
            Options::default()
        );
    }

    #[test]
    fn options_ignore_unknown_and_mistyped_keys() {
        let mut dict = HashMap::new();
        dict.insert("future-key".to_owned(), OwnedValue::from(7u32));
        dict.insert(ACTIVATION_TOKEN_KEY.to_owned(), OwnedValue::from(1u32));
        assert_eq!(Options::from_dict(&dict), Options::default());
    }

    #[test]
    fn status_round_trip() {
        let status = DaemonStatus {
            version: "0.1.0".into(),
            pid: 1234,
            capabilities: "layer-shell: v5\nportal: none".into(),
            popup_visible: true,
        };
        assert_eq!(DaemonStatus::from_dict(&status.to_dict()), status);
    }

    #[test]
    fn names_match_core_constants() {
        assert_eq!(BUS_NAME, "io.github.quicktranslate.QuickTranslate");
        assert_eq!(OBJECT_PATH, "/io/github/quicktranslate/QuickTranslate");
    }
}
