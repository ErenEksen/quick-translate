//! XDG GlobalShortcuts portal flow (design D6).
//!
//! Order matters: xdg-desktop-portal ties an app ID to a D-Bus connection on its first
//! portal call (including property reads), so `Registry.Register` must come first on the
//! connection used for the session.

use std::{fmt, future::Future, time::Duration};

use ashpd::{
    ActivationToken, AppID,
    desktop::{
        CreateSessionOptions, ResponseError, Session,
        global_shortcuts::{ConfigureShortcutsOptions, GlobalShortcuts, NewShortcut, Shortcut},
    },
};
use futures::{Stream, StreamExt, future, stream::BoxStream};
use quick_translate_core::APP_ID;
use tokio::{sync::watch, task::JoinHandle};
use zbus::{names::InterfaceName, zvariant::OwnedValue};

use super::{Activation, PREFERRED_TRIGGER, SHORTCUT_DESCRIPTION, SHORTCUT_ID};

pub const PORTAL_DESTINATION: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub const GLOBAL_SHORTCUTS_INTERFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
pub const REGISTRY_INTERFACE: &str = "org.freedesktop.host.portal.Registry";

/// Upper bound for portal calls that never involve user interaction.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Stream of shortcut activations from the portal.
pub type ActivationStream = BoxStream<'static, Activation>;

#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    #[error("the GlobalShortcuts portal interface is not available")]
    Unavailable,
    #[error("{step} failed: {source}")]
    Call {
        step: &'static str,
        #[source]
        source: Box<ashpd::Error>,
    },
    #[error("{step} timed out")]
    Timeout { step: &'static str },
}

impl PortalError {
    /// Whether the host portal Registry call is what failed.
    pub fn is_registry(&self) -> bool {
        matches!(
            self,
            Self::Call {
                step: STEP_REGISTER,
                ..
            } | Self::Timeout {
                step: STEP_REGISTER
            }
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigureError {
    /// `ConfigureShortcuts` needs interface version 2. Tell the user to rebind the shortcut
    /// in the system shortcut settings instead.
    #[error(
        "GlobalShortcuts version {version} cannot open the shortcut configuration; \
         change it in the system shortcut settings"
    )]
    Unsupported { version: u32 },
    #[error("ConfigureShortcuts failed: {0}")]
    Portal(#[source] Box<ashpd::Error>),
}

const STEP_REGISTER: &str = "Registry.Register";

/// Read the `version` property of `org.freedesktop.portal.GlobalShortcuts`.
///
/// Returns `None` when the portal or the interface is missing. Because this is a portal
/// call, use a dedicated probe connection, or call it after `Registry.Register`.
pub async fn detect_version(connection: &zbus::Connection) -> Option<u32> {
    let read = async {
        let props =
            zbus::fdo::PropertiesProxy::new(connection, PORTAL_DESTINATION, PORTAL_PATH).await?;
        let interface = InterfaceName::from_static_str_unchecked(GLOBAL_SHORTCUTS_INTERFACE);
        let value = props.get(interface, "version").await?;
        Ok::<_, zbus::Error>(u32::try_from(value)?)
    };
    match tokio::time::timeout(CALL_TIMEOUT, read).await {
        Ok(Ok(version)) => Some(version),
        Ok(Err(err)) => {
            tracing::debug!("GlobalShortcuts portal not detected: {err}");
            None
        }
        Err(_) => {
            tracing::debug!("GlobalShortcuts portal version read timed out");
            None
        }
    }
}

/// A live GlobalShortcuts session. Keep it for the daemon's lifetime and [`close`] it on quit.
///
/// [`close`]: PortalShortcuts::close
pub struct PortalShortcuts {
    proxy: GlobalShortcuts,
    session: Session<GlobalShortcuts>,
    version: u32,
    trigger: watch::Receiver<Option<String>>,
    /// Behind a mutex only so the session is `Sync` and can be shared with tasks.
    activations: std::sync::Mutex<Option<ActivationStream>>,
    watcher: JoinHandle<()>,
}

impl PortalShortcuts {
    /// Run the full flow: `Registry.Register`, version check, `CreateSession`,
    /// `BindShortcuts` (on every start; persisted bindings are kept by the portal) and
    /// `ListShortcuts`. Signal subscriptions are set up before binding, so no activation is
    /// lost.
    pub async fn start(connection: &zbus::Connection) -> Result<Self, PortalError> {
        let app_id = AppID::try_from(APP_ID).map_err(|source| PortalError::Call {
            step: STEP_REGISTER,
            source: source.into(),
        })?;
        call(
            STEP_REGISTER,
            ashpd::register_host_app_with_connection(connection.clone(), app_id),
        )
        .await?;

        let version = detect_version(connection)
            .await
            .ok_or(PortalError::Unavailable)?;
        let proxy = call(
            "GlobalShortcuts proxy",
            GlobalShortcuts::with_connection(connection.clone()),
        )
        .await?;
        let session = call(
            "CreateSession",
            proxy.create_session(CreateSessionOptions::default()),
        )
        .await?;

        let (trigger, activations, changed) = match Self::bind(&proxy, &session, version).await {
            Ok(bound) => bound,
            Err(err) => {
                if let Err(close_err) = session.close().await {
                    tracing::debug!("closing the failed shortcut session: {close_err}");
                }
                return Err(err);
            }
        };

        let (tx, rx) = watch::channel(trigger);
        let watcher = tokio::spawn(async move {
            let mut changed = std::pin::pin!(changed);
            while let Some(trigger) = changed.next().await {
                tracing::info!("portal shortcut trigger changed to {trigger:?}");
                tx.send_replace(trigger);
            }
        });
        Ok(Self {
            proxy,
            session,
            version,
            trigger: rx,
            activations: std::sync::Mutex::new(Some(activations)),
            watcher,
        })
    }

    async fn bind(
        proxy: &GlobalShortcuts,
        session: &Session<GlobalShortcuts>,
        version: u32,
    ) -> Result<
        (
            Option<String>,
            ActivationStream,
            impl Stream<Item = Option<String>> + Send + 'static,
        ),
        PortalError,
    > {
        let activations = call("subscribe Activated", proxy.receive_activated())
            .await?
            .filter_map(|signal| {
                future::ready((signal.shortcut_id() == SHORTCUT_ID).then(|| Activation {
                    token: activation_token(signal.options().get("activation_token")),
                }))
            })
            .boxed();
        let changed = call(
            "subscribe ShortcutsChanged",
            proxy.receive_shortcuts_changed(),
        )
        .await?
        .filter_map(|signal| {
            future::ready(
                signal
                    .shortcuts()
                    .iter()
                    .find(|s| s.id() == SHORTCUT_ID)
                    .map(|s| non_empty(s.trigger_description())),
            )
        });

        let shortcut = NewShortcut::new(SHORTCUT_ID, SHORTCUT_DESCRIPTION)
            .preferred_trigger(PREFERRED_TRIGGER);
        // No timeout: the portal may be waiting on a confirmation dialog.
        let bound = match proxy
            .bind_shortcuts(session, &[shortcut], None, Default::default())
            .await
            .and_then(|request| request.response())
        {
            Ok(response) => trigger_of(response.shortcuts()),
            Err(ashpd::Error::Response(ResponseError::Cancelled)) => {
                tracing::warn!("the shortcut binding was declined in the system dialog");
                None
            }
            Err(source) => {
                return Err(PortalError::Call {
                    step: "BindShortcuts",
                    source: source.into(),
                });
            }
        };

        let listed = call("ListShortcuts", async {
            proxy
                .list_shortcuts(session, Default::default())
                .await?
                .response()
        })
        .await;
        let trigger = match listed {
            Ok(list) => trigger_of(list.shortcuts()),
            Err(err) => {
                tracing::warn!("{err}; showing the trigger from BindShortcuts");
                bound
            }
        };
        tracing::info!("portal shortcut bound (GlobalShortcuts v{version}), trigger {trigger:?}");
        Ok((trigger, activations, changed))
    }

    /// GlobalShortcuts interface version reported by the portal.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Whether "Change shortcut" can open the portal's configuration UI (version 2 or later).
    pub fn can_configure(&self) -> bool {
        self.version >= 2
    }

    /// The trigger the portal assigned, as its human-readable description.
    pub fn current_trigger(&self) -> Option<String> {
        self.trigger.borrow().clone()
    }

    /// A receiver that observes trigger changes reported by `ShortcutsChanged`.
    pub fn watch_trigger(&self) -> watch::Receiver<Option<String>> {
        self.trigger.clone()
    }

    /// Stream of trigger changes, starting after the current value.
    pub fn trigger_changes(&self) -> impl Stream<Item = Option<String>> + Send + 'static {
        let mut rx = self.trigger.clone();
        rx.mark_unchanged();
        futures::stream::unfold(rx, |mut rx| async move {
            rx.changed().await.ok()?;
            let value = rx.borrow_and_update().clone();
            Some((value, rx))
        })
    }

    /// Take the activation stream. Returns `None` after the first call.
    pub fn take_activations(&mut self) -> Option<ActivationStream> {
        self.activations
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Open the portal's shortcut configuration UI (`ConfigureShortcuts`, version 2+).
    pub async fn configure(&self, activation_token: Option<String>) -> Result<(), ConfigureError> {
        if !self.can_configure() {
            return Err(ConfigureError::Unsupported {
                version: self.version,
            });
        }
        let options = ConfigureShortcutsOptions::default()
            .set_activation_token(activation_token.map(ActivationToken::from));
        self.proxy
            .configure_shortcuts(&self.session, None, options)
            .await
            .map_err(|err| ConfigureError::Portal(err.into()))
    }

    /// Close the session, which unregisters the shortcut for this run.
    pub async fn close(self) -> Result<(), PortalError> {
        self.watcher.abort();
        call("Session.Close", self.session.close()).await
    }
}

impl Drop for PortalShortcuts {
    fn drop(&mut self) {
        self.watcher.abort();
    }
}

impl fmt::Debug for PortalShortcuts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PortalShortcuts")
            .field("session", &self.session)
            .field("version", &self.version)
            .field("trigger", &*self.trigger.borrow())
            .finish_non_exhaustive()
    }
}

async fn call<T>(
    step: &'static str,
    fut: impl Future<Output = Result<T, ashpd::Error>>,
) -> Result<T, PortalError> {
    match tokio::time::timeout(CALL_TIMEOUT, fut).await {
        Ok(result) => result.map_err(|source| PortalError::Call {
            step,
            source: source.into(),
        }),
        Err(_) => Err(PortalError::Timeout { step }),
    }
}

fn trigger_of(shortcuts: &[Shortcut]) -> Option<String> {
    shortcuts
        .iter()
        .find(|s| s.id() == SHORTCUT_ID)
        .and_then(|s| non_empty(s.trigger_description()))
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

fn activation_token(value: Option<&OwnedValue>) -> Option<String> {
    value
        .and_then(|v| v.downcast_ref::<String>().ok())
        .filter(|t| !t.is_empty())
}
