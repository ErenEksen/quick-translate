//! Blocking CLI-side forwarding. Never creates a Tokio runtime or spawns a thread.

use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Duration;

use zbus::zvariant::OwnedValue;

use crate::wire::{self, Call, WireError};
use crate::{BUS_NAME, DaemonStatus, INTERFACE, OBJECT_PATH, Options, socket};

/// A command for the running daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Toggle(Options),
    Show(Options),
    Settings,
    Quit,
    Status,
}

impl Request {
    pub(crate) fn member(&self) -> &'static str {
        match self {
            Self::Toggle(_) => "Toggle",
            Self::Show(_) => "Show",
            Self::Settings => "Settings",
            Self::Quit => "Quit",
            Self::Status => "Status",
        }
    }
}

/// The daemon's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Done,
    Status(DaemonStatus),
}

#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    #[error("the quick-translate daemon is not running")]
    NotRunning,
    #[error("the daemon returned {name}: {message}")]
    Remote { name: String, message: String },
    #[error("IPC with the daemon failed: {0}")]
    Io(#[from] io::Error),
    #[error("IPC protocol error: {0}")]
    Protocol(String),
}

/// Where to look for the daemon.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// Session bus address; `None` skips D-Bus.
    pub bus_address: Option<String>,
    /// Unix-socket fallback; `None` skips it.
    pub socket: Option<PathBuf>,
    pub timeout: Duration,
}

impl Endpoints {
    /// `DBUS_SESSION_BUS_ADDRESS` (or `$XDG_RUNTIME_DIR/bus`) and `$XDG_RUNTIME_DIR/quick-translate.sock`.
    pub fn from_env() -> Self {
        let bus_address = std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .ok()
            .filter(|address| !address.is_empty())
            .or_else(|| {
                // Same fallback as zbus, which the daemon uses.
                let runtime = std::env::var_os("XDG_RUNTIME_DIR")
                    .filter(|dir| !dir.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        PathBuf::from(format!("/run/user/{}", rustix::process::geteuid().as_raw()))
                    });
                let path = runtime.join("bus");
                Some(format!(
                    "unix:path={}",
                    wire::escape(path.as_os_str().as_bytes())
                ))
            });
        Self {
            bus_address,
            socket: socket::default_path(),
            timeout: Duration::from_secs(5),
        }
    }
}

/// Sends `request` to the daemon over the session bus, falling back to the Unix socket when the
/// bus is unreachable or the daemon is not on it.
pub fn forward(request: &Request, endpoints: &Endpoints) -> Result<Reply, ForwardError> {
    if let Some(address) = &endpoints.bus_address {
        match forward_bus(address, request, endpoints.timeout) {
            Err(ForwardError::NotRunning) => {}
            result => return result,
        }
    }
    match &endpoints.socket {
        Some(path) => socket::request(path, request, endpoints.timeout),
        None => Err(ForwardError::NotRunning),
    }
}

fn forward_bus(address: &str, request: &Request, timeout: Duration) -> Result<Reply, ForwardError> {
    let call = Call {
        destination: BUS_NAME,
        path: OBJECT_PATH,
        interface: INTERFACE,
        member: request.member(),
    };
    let result = match request {
        Request::Toggle(options) | Request::Show(options) => {
            wire::call(address, timeout, &call, &(options.to_dict(),))
        }
        Request::Settings | Request::Quit | Request::Status => {
            wire::call(address, timeout, &call, &())
        }
    };
    match result {
        Ok(returned) => match request {
            Request::Status => {
                let dict: HashMap<String, OwnedValue> = returned.body().map_err(protocol)?;
                Ok(Reply::Status(DaemonStatus::from_dict(&dict)))
            }
            _ => Ok(Reply::Done),
        },
        Err(WireError::Unavailable(err)) => {
            tracing::debug!(%err, address, "session bus unreachable");
            Err(ForwardError::NotRunning)
        }
        Err(WireError::Remote { name, .. })
            if matches!(
                name.as_str(),
                "org.freedesktop.DBus.Error.ServiceUnknown"
                    | "org.freedesktop.DBus.Error.NameHasNoOwner"
            ) =>
        {
            Err(ForwardError::NotRunning)
        }
        // The daemon may exit before its reply to `Quit` is flushed.
        Err(WireError::Remote { name, .. })
            if *request == Request::Quit && name == "org.freedesktop.DBus.Error.NoReply" =>
        {
            Ok(Reply::Done)
        }
        Err(WireError::Io(err))
            if *request == Request::Quit && err.kind() == ErrorKind::UnexpectedEof =>
        {
            Ok(Reply::Done)
        }
        Err(WireError::Remote { name, message }) => Err(ForwardError::Remote { name, message }),
        Err(WireError::Io(err)) => Err(ForwardError::Io(err)),
        Err(WireError::Protocol(message)) => Err(ForwardError::Protocol(message)),
    }
}

fn protocol(err: WireError) -> ForwardError {
    match err {
        WireError::Protocol(message) => ForwardError::Protocol(message),
        other => ForwardError::Protocol(format!("{other:?}")),
    }
}
