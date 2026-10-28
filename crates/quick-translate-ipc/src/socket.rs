//! Unix-socket fallback used only when no session bus is reachable.
//!
//! One request line per connection, `\t`-separated and %-escaped:
//! `toggle[\t<token>]`, `show[\t<token>]`, `settings`, `quit`, `status`.
//! Replies are `ok[\t<key>=<value>...]` or `err\t<message>`.
//! Single instance is guaranteed by an `flock` on `<socket>.lock`, held for the daemon's
//! lifetime, which also makes removing a stale socket safe.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::fs::{FlockOperation, flock};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::client::{ForwardError, Reply, Request};
use crate::{DaemonCommand, DaemonStatus, Options};

const MAX_LINE: usize = 8 * 1024;

/// `$XDG_RUNTIME_DIR/quick-translate.sock`.
pub fn default_path() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty())?;
    Some(PathBuf::from(runtime).join("quick-translate.sock"))
}

fn lock_path(socket: &Path) -> PathBuf {
    let mut name = socket.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Result of [`claim`].
#[derive(Debug)]
pub enum SocketClaimOutcome {
    Claimed(SocketClaim),
    AlreadyRunning,
}

/// The bound socket plus the instance lock. Dropping it removes the socket file.
#[derive(Debug)]
pub struct SocketClaim {
    listener: UnixListener,
    path: PathBuf,
    _lock: File,
}

impl SocketClaim {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A Tokio listener for [`serve`]; must be called inside a Tokio runtime.
    pub fn tokio_listener(&self) -> io::Result<tokio::net::UnixListener> {
        let listener = self.listener.try_clone()?;
        listener.set_nonblocking(true)?;
        tokio::net::UnixListener::from_std(listener)
    }
}

impl Drop for SocketClaim {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Takes the instance lock and binds `path`, replacing a stale socket left by a dead daemon.
pub fn claim(path: &Path) -> io::Result<SocketClaimOutcome> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path(path))?;
    match flock(&lock, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(rustix::io::Errno::WOULDBLOCK) => return Ok(SocketClaimOutcome::AlreadyRunning),
        Err(err) => return Err(err.into()),
    }
    match fs::remove_file(path) {
        Ok(()) => tracing::debug!(path = %path.display(), "removed stale socket"),
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let listener = UnixListener::bind(path)?;
    Ok(SocketClaimOutcome::Claimed(SocketClaim {
        listener,
        path: path.to_owned(),
        _lock: lock,
    }))
}

/// Accepts connections until the command channel closes.
pub async fn serve(listener: tokio::net::UnixListener, commands: mpsc::Sender<DaemonCommand>) {
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(err) => {
                    tracing::warn!(%err, "socket accept failed");
                    continue;
                }
            },
            () = commands.closed() => return,
        };
        let commands = commands.clone();
        tokio::spawn(async move {
            if let Err(err) = handle(stream, commands).await {
                tracing::debug!(%err, "socket client failed");
            }
        });
    }
}

async fn handle(
    stream: tokio::net::UnixStream,
    commands: mpsc::Sender<DaemonCommand>,
) -> io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read).take(MAX_LINE as u64);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let response = match dispatch(line.trim_end_matches(['\r', '\n']), &commands).await {
        Ok(fields) => {
            let mut out = String::from("ok");
            for (key, value) in fields {
                out.push('\t');
                out.push_str(key);
                out.push('=');
                out.push_str(&escape(&value));
            }
            out
        }
        Err(message) => format!("err\t{}", escape(&message)),
    };
    write.write_all(response.as_bytes()).await?;
    write.write_all(b"\n").await?;
    write.shutdown().await
}

async fn dispatch(
    line: &str,
    commands: &mpsc::Sender<DaemonCommand>,
) -> Result<Vec<(&'static str, String)>, String> {
    let mut parts = line.split('\t');
    let verb = parts.next().unwrap_or_default();
    let options = || {
        let activation_token = parts
            .clone()
            .next()
            .map(unescape)
            .filter(|token| !token.is_empty());
        Options { activation_token }
    };
    let (command, status) = match verb {
        "toggle" => (DaemonCommand::Toggle { options: options() }, None),
        "show" => (DaemonCommand::Show { options: options() }, None),
        "settings" => (DaemonCommand::Settings, None),
        "quit" => (DaemonCommand::Quit, None),
        "status" => {
            let (reply, receiver) = oneshot::channel();
            (DaemonCommand::Status { reply }, Some(receiver))
        }
        other => return Err(format!("unknown command {other:?}")),
    };
    commands
        .send(command)
        .await
        .map_err(|_| "daemon is shutting down".to_owned())?;
    let Some(receiver) = status else {
        return Ok(Vec::new());
    };
    let status = receiver
        .await
        .map_err(|_| "daemon dropped the status request".to_owned())?;
    Ok(vec![
        (DaemonStatus::VERSION, status.version),
        (DaemonStatus::PID, status.pid.to_string()),
        (DaemonStatus::CAPABILITIES, status.capabilities),
        (
            DaemonStatus::POPUP_VISIBLE,
            status.popup_visible.to_string(),
        ),
    ])
}

/// Blocking client side, used by [`crate::forward`].
pub(crate) fn request(
    path: &Path,
    request: &Request,
    timeout: Duration,
) -> Result<Reply, ForwardError> {
    let mut stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(ForwardError::NotRunning);
        }
        Err(err) => return Err(err.into()),
    };
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut line = match request {
        Request::Toggle(options) | Request::Show(options) => {
            let verb = if matches!(request, Request::Toggle(_)) {
                "toggle"
            } else {
                "show"
            };
            match &options.activation_token {
                Some(token) => format!("{verb}\t{}", escape(token)),
                None => verb.to_owned(),
            }
        }
        Request::Settings => "settings".to_owned(),
        Request::Quit => "quit".to_owned(),
        Request::Status => "status".to_owned(),
    };
    line.push('\n');
    stream.write_all(line.as_bytes())?;

    let mut response = String::new();
    let read = BufReader::new(&stream)
        .take(MAX_LINE as u64)
        .read_line(&mut response);
    match read {
        Ok(0) if *request == Request::Quit => return Ok(Reply::Done),
        Ok(0) => return Err(ForwardError::Protocol("daemon closed the socket".into())),
        Ok(_) => {}
        Err(err) => return Err(err.into()),
    }
    let mut fields = response.trim_end_matches(['\r', '\n']).split('\t');
    match fields.next() {
        Some("ok") if *request == Request::Status => {
            let mut status = DaemonStatus::default();
            for field in fields {
                let Some((key, value)) = field.split_once('=') else {
                    continue;
                };
                let value = unescape(value);
                match key {
                    DaemonStatus::VERSION => status.version = value,
                    DaemonStatus::PID => status.pid = value.parse().unwrap_or_default(),
                    DaemonStatus::CAPABILITIES => status.capabilities = value,
                    DaemonStatus::POPUP_VISIBLE => status.popup_visible = value == "true",
                    _ => {}
                }
            }
            Ok(Reply::Status(status))
        }
        Some("ok") => Ok(Reply::Done),
        Some("err") => Err(ForwardError::Remote {
            name: "socket".into(),
            message: fields.next().map(unescape).unwrap_or_default(),
        }),
        _ => Err(ForwardError::Protocol(format!(
            "unexpected socket reply {response:?}"
        ))),
    }
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '%' => out.push_str("%25"),
            '\t' => out.push_str("%09"),
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(value: &str) -> String {
    value
        .replace("%09", "\t")
        .replace("%0A", "\n")
        .replace("%0D", "\r")
        .replace("%25", "%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_round_trips() {
        for text in ["plain", "a\tb\nc\rd", "100%", "%09 literal", ""] {
            assert_eq!(unescape(&escape(text)), text, "{text:?}");
            assert!(!escape(text).contains(['\t', '\n', '\r']));
        }
    }

    #[test]
    fn claim_is_exclusive_and_cleans_up() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("qt.sock");
        // A stale socket file from a crashed daemon.
        drop(UnixListener::bind(&path).expect("bind"));
        assert!(path.exists());

        let first = match claim(&path).expect("claim") {
            SocketClaimOutcome::Claimed(claim) => claim,
            SocketClaimOutcome::AlreadyRunning => panic!("stale socket blocked the claim"),
        };
        assert!(matches!(
            claim(&path).expect("second claim"),
            SocketClaimOutcome::AlreadyRunning
        ));
        drop(first);
        assert!(!path.exists());
        assert!(matches!(
            claim(&path).expect("reclaim"),
            SocketClaimOutcome::Claimed(_)
        ));
    }

    #[test]
    fn missing_socket_means_not_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = request(
            &dir.path().join("none.sock"),
            &Request::Status,
            Duration::from_secs(1),
        );
        assert!(matches!(result, Err(ForwardError::NotRunning)));
    }
}
