//! Daemon side of the D-Bus interface.

use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot};
use zbus::fdo::{self, RequestNameFlags, RequestNameReply};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;
use zbus::{Connection, interface};

use crate::settings::parse_backend;
use crate::{BUS_NAME, DaemonCommand, DaemonStatus, OBJECT_PATH, Options, SecretString};

/// The exported object. Each method call is turned into a [`DaemonCommand`].
pub struct QuickTranslateInterface {
    commands: mpsc::Sender<DaemonCommand>,
}

impl QuickTranslateInterface {
    pub fn new(commands: mpsc::Sender<DaemonCommand>) -> Self {
        Self { commands }
    }

    async fn send(&self, command: DaemonCommand) -> fdo::Result<()> {
        self.commands
            .send(command)
            .await
            .map_err(|_| fdo::Error::Failed("daemon is shutting down".into()))
    }

    async fn request<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<T>) -> DaemonCommand,
    ) -> fdo::Result<T> {
        let (reply, receiver) = oneshot::channel();
        self.send(command(reply)).await?;
        receiver
            .await
            .map_err(|_| fdo::Error::Failed("daemon dropped the request".into()))
    }
}

fn failed(result: Result<(), String>) -> fdo::Result<()> {
    result.map_err(fdo::Error::Failed)
}

#[interface(name = "io.github.quicktranslate.QuickTranslate")]
impl QuickTranslateInterface {
    async fn toggle(&self, options: HashMap<String, OwnedValue>) -> fdo::Result<()> {
        let options = Options::from_dict(&options);
        self.send(DaemonCommand::Toggle { options }).await
    }

    async fn show(&self, options: HashMap<String, OwnedValue>) -> fdo::Result<()> {
        let options = Options::from_dict(&options);
        self.send(DaemonCommand::Show { options }).await
    }

    async fn settings(&self) -> fdo::Result<()> {
        self.send(DaemonCommand::Settings).await
    }

    async fn quit(&self) -> fdo::Result<()> {
        self.send(DaemonCommand::Quit).await
    }

    async fn status(&self) -> fdo::Result<HashMap<String, OwnedValue>> {
        let (reply, receiver) = oneshot::channel();
        self.send(DaemonCommand::Status { reply }).await?;
        let status: DaemonStatus = receiver
            .await
            .map_err(|_| fdo::Error::Failed("daemon dropped the status request".into()))?;
        Ok(status.to_dict())
    }

    async fn reload_config(&self) -> fdo::Result<()> {
        failed(
            self.request(|reply| DaemonCommand::ReloadConfig { reply })
                .await?,
        )
    }

    async fn set_session_secret(&self, backend: &str, key: &str) -> fdo::Result<()> {
        let backend = parse_backend(backend)
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("unknown backend {backend:?}")))?;
        self.send(DaemonCommand::SetSessionSecret {
            backend,
            key: SecretString::new(key),
        })
        .await
    }

    async fn shortcut_info(&self) -> fdo::Result<HashMap<String, OwnedValue>> {
        let info = self
            .request(|reply| DaemonCommand::ShortcutInfo { reply })
            .await?;
        Ok(info.to_dict())
    }

    async fn configure_shortcut(&self, activation_token: &str) -> fdo::Result<()> {
        let activation_token = Some(activation_token.to_owned()).filter(|t| !t.is_empty());
        failed(
            self.request(|reply| DaemonCommand::ConfigureShortcut {
                activation_token,
                reply,
            })
            .await?,
        )
    }

    async fn set_evdev_enabled(&self, enabled: bool) -> fdo::Result<()> {
        failed(
            self.request(|reply| DaemonCommand::SetEvdevEnabled { enabled, reply })
                .await?,
        )
    }

    /// The portal reported a new trigger (empty when unassigned).
    #[zbus(signal)]
    pub async fn shortcut_changed(emitter: &SignalEmitter<'_>, trigger: &str) -> zbus::Result<()>;

    /// `Settings()` was called while the settings window is already open: raise it.
    #[zbus(signal)]
    pub async fn settings_requested(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Emits `ShortcutChanged(trigger)` from the daemon object.
pub async fn emit_shortcut_changed(conn: &Connection, trigger: Option<&str>) -> zbus::Result<()> {
    let emitter = SignalEmitter::new(conn, OBJECT_PATH)?;
    QuickTranslateInterface::shortcut_changed(&emitter, trigger.unwrap_or_default()).await
}

/// Emits `SettingsRequested()` from the daemon object.
pub async fn emit_settings_requested(conn: &Connection) -> zbus::Result<()> {
    let emitter = SignalEmitter::new(conn, OBJECT_PATH)?;
    QuickTranslateInterface::settings_requested(&emitter).await
}

/// Result of [`claim_bus_name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusClaim {
    /// This connection owns the name and serves the interface.
    Owner,
    /// Another daemon owns the name; nothing was left exported.
    AlreadyRunning,
}

/// Exports the interface and requests the well-known name with `DoNotQueue`, so exactly one of
/// several racing daemons wins. The object is exported first so no early call is lost.
pub async fn claim_bus_name(
    conn: &Connection,
    commands: mpsc::Sender<DaemonCommand>,
) -> zbus::Result<BusClaim> {
    let server = conn.object_server();
    server
        .at(OBJECT_PATH, QuickTranslateInterface::new(commands))
        .await?;
    let reply = conn
        .request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())
        .await;
    match reply {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => Ok(BusClaim::Owner),
        Ok(RequestNameReply::Exists | RequestNameReply::InQueue) | Err(zbus::Error::NameTaken) => {
            server
                .remove::<QuickTranslateInterface, _>(OBJECT_PATH)
                .await?;
            Ok(BusClaim::AlreadyRunning)
        }
        Err(err) => Err(err),
    }
}
