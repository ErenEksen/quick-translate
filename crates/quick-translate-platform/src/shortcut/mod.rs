//! Global shortcut: portal flow, compositor snippets and opt-in evdev (design D6).
//!
//! [`start`] picks the mechanism once per daemon start. Every trigger, whether a portal
//! activation, a `--toggle` request or an evdev combo, ends up as an [`Activation`].

pub mod evdev;
pub mod portal;
pub mod snippet;

use std::fmt;

use quick_translate_core::Capabilities;

pub use portal::{ConfigureError, PortalError, PortalShortcuts};
pub use snippet::{Snippet, SnippetFormat, ToggleCommand};

/// Shortcut id bound through the portal.
pub const SHORTCUT_ID: &str = "translate-selection";
/// Human-readable description shown in the system's shortcut settings.
pub const SHORTCUT_DESCRIPTION: &str = "Translate the selected text";
/// Preferred trigger in the XDG shortcuts format.
pub const PREFERRED_TRIGGER: &str = "CTRL+ALT+c";

/// One shortcut press.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activation {
    /// XDG activation token, when the trigger provided one (portal v2).
    pub token: Option<String>,
}

/// The active shortcut mechanism.
pub enum Mechanism {
    /// The GlobalShortcuts portal session. Keep it for the daemon's lifetime.
    Portal(PortalShortcuts),
    /// No usable portal: the user binds `<exe> --toggle` in the compositor.
    Compositor(Snippet),
}

impl Mechanism {
    /// Label for the settings window.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Portal(_) => "System shortcut (portal)",
            Self::Compositor(_) => "Compositor binding (configure it in your compositor)",
        }
    }
}

impl fmt::Debug for Mechanism {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Portal(p) => f.debug_tuple("Portal").field(p).finish(),
            Self::Compositor(s) => f.debug_tuple("Compositor").field(s).finish(),
        }
    }
}

/// Choose and set up the shortcut mechanism.
///
/// Uses the portal when `caps.portal_global_shortcuts` is set and the full flow
/// (`Registry.Register`, `CreateSession`, `BindShortcuts`, `ListShortcuts`) succeeds on
/// `connection`. Any failure is logged and yields the compositor snippet for
/// `caps.compositor`; this never fails.
///
/// `BindShortcuts` may wait for a system confirmation dialog on first use, so run this
/// off the activation hot path.
pub async fn start(
    connection: Option<&zbus::Connection>,
    caps: &Capabilities,
    command: &ToggleCommand,
) -> Mechanism {
    let fallback = || Mechanism::Compositor(snippet::snippet_for(&caps.compositor, command));
    if caps.portal_global_shortcuts.is_none() {
        tracing::info!(
            "GlobalShortcuts portal not available, using the compositor-binding fallback"
        );
        return fallback();
    }
    let Some(connection) = connection else {
        tracing::info!("no session bus, using the compositor-binding fallback");
        return fallback();
    };
    match PortalShortcuts::start(connection).await {
        Ok(portal) => Mechanism::Portal(portal),
        Err(err) => {
            tracing::warn!(
                "portal shortcut setup failed, using the compositor-binding fallback: {err}"
            );
            fallback()
        }
    }
}
