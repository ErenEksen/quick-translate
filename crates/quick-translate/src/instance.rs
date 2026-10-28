//! Single-instance claim: the session-bus name, or the Unix socket when there is no bus.

use anyhow::Context as _;
use futures::StreamExt as _;
use quick_translate_ipc::socket::{self, SocketClaim, SocketClaimOutcome};
use quick_translate_ipc::{BUS_NAME, BusClaim, DaemonCommand, claim_bus_name};
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zbus::message::Type as MessageType;
use zbus::{Connection, MatchRule, MessageStream};

/// The claimed daemon identity. Dropping or [`Instance::release`] gives it up.
pub enum Instance {
    Bus {
        conn: Connection,
        /// Yields on `NameLost` and ends when the bus connection closes.
        watch: Pin<Box<MessageStream>>,
    },
    Socket {
        claim: SocketClaim,
        server: JoinHandle<()>,
    },
}

/// Claims the instance, or returns `None` when another daemon already holds it.
pub async fn claim(commands: mpsc::Sender<DaemonCommand>) -> anyhow::Result<Option<Instance>> {
    let conn = match Connection::session().await {
        Ok(conn) => conn,
        Err(err) => {
            tracing::info!(%err, "no session bus, using the socket fallback");
            return claim_socket(commands);
        }
    };
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameLost")?
        .add_arg(BUS_NAME)?
        .build();
    let watch = MessageStream::for_match_rule(rule, &conn, Some(4)).await?;
    match claim_bus_name(&conn, commands)
        .await
        .context("cannot claim the bus name")?
    {
        BusClaim::Owner => {
            tracing::info!(name = BUS_NAME, "daemon owns the bus name");
            Ok(Some(Instance::Bus {
                conn,
                watch: Box::pin(watch),
            }))
        }
        BusClaim::AlreadyRunning => Ok(None),
    }
}

fn claim_socket(commands: mpsc::Sender<DaemonCommand>) -> anyhow::Result<Option<Instance>> {
    let path = socket::default_path()
        .context("no session bus and XDG_RUNTIME_DIR is not set for the socket fallback")?;
    match socket::claim(&path).with_context(|| format!("cannot bind {}", path.display()))? {
        SocketClaimOutcome::Claimed(claim) => {
            let server = tokio::spawn(socket::serve(claim.tokio_listener()?, commands));
            tracing::info!(path = %claim.path().display(), "daemon listens on the socket");
            Ok(Some(Instance::Socket { claim, server }))
        }
        SocketClaimOutcome::AlreadyRunning => Ok(None),
    }
}

impl Instance {
    /// The session-bus connection that owns the name, for emitting signals.
    pub fn connection(&self) -> Option<&Connection> {
        match self {
            Self::Bus { conn, .. } => Some(conn),
            Self::Socket { .. } => None,
        }
    }

    /// Resolves when the identity is lost (bus connection closed or name taken away).
    pub async fn lost(&mut self) {
        match self {
            Self::Bus { watch, .. } => {
                let _ = watch.next().await;
            }
            Self::Socket { .. } => std::future::pending().await,
        }
    }

    pub async fn release(self) {
        match self {
            Self::Bus { conn, watch } => {
                drop(watch);
                if let Err(err) = conn.release_name(BUS_NAME).await {
                    tracing::debug!(%err, "release_name failed");
                }
                let _ =
                    tokio::time::timeout(Duration::from_secs(1), conn.graceful_shutdown()).await;
            }
            Self::Socket { claim, server } => {
                server.abort();
                drop(claim);
            }
        }
    }
}
