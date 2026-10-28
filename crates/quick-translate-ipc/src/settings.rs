//! Types for the calls the settings process makes to the daemon (`ReloadConfig`,
//! `SetSessionSecret`, `ShortcutInfo`, `ConfigureShortcut`, `SetEvdevEnabled`).

use std::collections::HashMap;
use std::fmt;

use quick_translate_core::BackendKind;
use zbus::zvariant::OwnedValue;

use crate::owned_str;

/// An API key in transit. `Debug` never shows it. Empty means "clear".
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_empty() {
            "<empty>"
        } else {
            "<redacted>"
        })
    }
}

/// The `backend` argument of `SetSessionSecret`: `deepl` or `openai`.
pub fn backend_name(backend: BackendKind) -> &'static str {
    match backend {
        BackendKind::Deepl => "deepl",
        BackendKind::Openai => "openai",
    }
}

/// Parses a [`backend_name`].
pub fn parse_backend(name: &str) -> Option<BackendKind> {
    match name {
        "deepl" => Some(BackendKind::Deepl),
        "openai" => Some(BackendKind::Openai),
        _ => None,
    }
}

/// How the global shortcut is delivered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShortcutMechanism {
    /// The GlobalShortcuts portal session.
    Portal,
    /// The user binds `<exe> --toggle` in the compositor.
    #[default]
    Compositor,
}

impl ShortcutMechanism {
    fn as_str(self) -> &'static str {
        match self {
            Self::Portal => "portal",
            Self::Compositor => "compositor",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "portal" => Self::Portal,
            _ => Self::Compositor,
        }
    }
}

/// A compositor binding snippet (compositor-binding mechanism).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnippetInfo {
    /// Environment name, e.g. `niri`.
    pub name: String,
    pub instructions: String,
    /// What the Copy button places on the clipboard.
    pub text: String,
}

/// Reply of `ShortcutInfo()`: everything the settings shortcut section shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShortcutInfo {
    pub mechanism: ShortcutMechanism,
    /// Label for the active mechanism, e.g. "System shortcut (portal)".
    pub label: String,
    /// GlobalShortcuts interface version, 0 without the portal.
    pub portal_version: u32,
    /// The trigger the portal assigned (portal mechanism), `None` when unassigned.
    pub trigger: Option<String>,
    /// `ConfigureShortcut` can open the portal's configuration UI (version 2 or later).
    pub can_configure: bool,
    /// The binding snippet (compositor mechanism).
    pub snippet: Option<SnippetInfo>,
    /// The input-device fallback is running.
    pub evdev_active: bool,
    /// Why the input-device fallback is not running although it was enabled.
    pub evdev_error: Option<String>,
    /// The warning shown before the user enables the input-device fallback.
    pub evdev_warning: String,
}

impl ShortcutInfo {
    pub const MECHANISM: &'static str = "mechanism";
    pub const LABEL: &'static str = "label";
    pub const PORTAL_VERSION: &'static str = "portal-version";
    pub const TRIGGER: &'static str = "trigger";
    pub const CAN_CONFIGURE: &'static str = "can-configure";
    pub const SNIPPET_NAME: &'static str = "snippet-name";
    pub const SNIPPET_INSTRUCTIONS: &'static str = "snippet-instructions";
    pub const SNIPPET_TEXT: &'static str = "snippet-text";
    pub const EVDEV_ACTIVE: &'static str = "evdev-active";
    pub const EVDEV_ERROR: &'static str = "evdev-error";
    pub const EVDEV_WARNING: &'static str = "evdev-warning";

    /// Optional strings are omitted when `None`.
    pub fn to_dict(&self) -> HashMap<String, OwnedValue> {
        let mut dict = HashMap::new();
        let mut put_str = |key: &str, value: &str| {
            dict.insert(key.to_owned(), OwnedValue::from(owned_str(value)));
        };
        put_str(Self::MECHANISM, self.mechanism.as_str());
        put_str(Self::LABEL, &self.label);
        put_str(Self::EVDEV_WARNING, &self.evdev_warning);
        if let Some(trigger) = &self.trigger {
            put_str(Self::TRIGGER, trigger);
        }
        if let Some(snippet) = &self.snippet {
            put_str(Self::SNIPPET_NAME, &snippet.name);
            put_str(Self::SNIPPET_INSTRUCTIONS, &snippet.instructions);
            put_str(Self::SNIPPET_TEXT, &snippet.text);
        }
        if let Some(error) = &self.evdev_error {
            put_str(Self::EVDEV_ERROR, error);
        }
        dict.insert(
            Self::PORTAL_VERSION.into(),
            OwnedValue::from(self.portal_version),
        );
        dict.insert(
            Self::CAN_CONFIGURE.into(),
            OwnedValue::from(self.can_configure),
        );
        dict.insert(
            Self::EVDEV_ACTIVE.into(),
            OwnedValue::from(self.evdev_active),
        );
        dict
    }

    /// Unknown keys are ignored and missing ones take their defaults.
    pub fn from_dict(dict: &HashMap<String, OwnedValue>) -> Self {
        let text = |key: &str| {
            dict.get(key)
                .and_then(|value| <&str>::try_from(value).ok())
                .map(str::to_owned)
        };
        let flag = |key: &str| {
            dict.get(key)
                .and_then(|value| bool::try_from(value).ok())
                .unwrap_or_default()
        };
        let snippet = text(Self::SNIPPET_TEXT).map(|text_value| SnippetInfo {
            name: text(Self::SNIPPET_NAME).unwrap_or_default(),
            instructions: text(Self::SNIPPET_INSTRUCTIONS).unwrap_or_default(),
            text: text_value,
        });
        Self {
            mechanism: ShortcutMechanism::parse(&text(Self::MECHANISM).unwrap_or_default()),
            label: text(Self::LABEL).unwrap_or_default(),
            portal_version: dict
                .get(Self::PORTAL_VERSION)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or_default(),
            trigger: text(Self::TRIGGER).filter(|t| !t.is_empty()),
            can_configure: flag(Self::CAN_CONFIGURE),
            snippet,
            evdev_active: flag(Self::EVDEV_ACTIVE),
            evdev_error: text(Self::EVDEV_ERROR).filter(|e| !e.is_empty()),
            evdev_warning: text(Self::EVDEV_WARNING).unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_info_round_trips() {
        let portal = ShortcutInfo {
            mechanism: ShortcutMechanism::Portal,
            label: "System shortcut (portal)".into(),
            portal_version: 2,
            trigger: Some("Ctrl+Alt+C".into()),
            can_configure: true,
            snippet: None,
            evdev_active: false,
            evdev_error: Some("no permission".into()),
            evdev_warning: "careful".into(),
        };
        assert_eq!(ShortcutInfo::from_dict(&portal.to_dict()), portal);

        let compositor = ShortcutInfo {
            mechanism: ShortcutMechanism::Compositor,
            label: "Compositor binding".into(),
            snippet: Some(SnippetInfo {
                name: "niri".into(),
                instructions: "Add to config.kdl".into(),
                text: "binds { }".into(),
            }),
            evdev_active: true,
            ..ShortcutInfo::default()
        };
        assert_eq!(ShortcutInfo::from_dict(&compositor.to_dict()), compositor);
        assert_eq!(
            ShortcutInfo::from_dict(&HashMap::new()),
            ShortcutInfo::default()
        );
    }

    #[test]
    fn secret_debug_is_redacted() {
        let secret = SecretString::new("sk-very-secret");
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert_eq!(format!("{:?}", SecretString::default()), "<empty>");
        assert_eq!(secret.expose(), "sk-very-secret");
    }

    #[test]
    fn backend_names_round_trip() {
        for backend in [BackendKind::Deepl, BackendKind::Openai] {
            assert_eq!(parse_backend(backend_name(backend)), Some(backend));
        }
        assert_eq!(parse_backend("google"), None);
    }
}
