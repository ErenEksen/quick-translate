//! Wayland-only gate (no X11 fallback).

use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

#[derive(Debug)]
pub enum GateError {
    Unset,
    Unreachable { socket: PathBuf, source: io::Error },
}

impl fmt::Display for GateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unset => f.write_str(
                "quick-translate requires a Wayland session: WAYLAND_DISPLAY is not set \
                 (X11 is not supported)",
            ),
            Self::Unreachable { socket, source } => write!(
                f,
                "quick-translate requires a Wayland session: cannot connect to the Wayland \
                 compositor at {}: {source}",
                socket.display()
            ),
        }
    }
}

/// Resolves `WAYLAND_DISPLAY` the way libwayland does: absolute, or relative to `XDG_RUNTIME_DIR`.
pub fn socket_path(display: &OsStr, runtime_dir: Option<&OsStr>) -> Result<PathBuf, GateError> {
    let display = PathBuf::from(display);
    if display.is_absolute() {
        return Ok(display);
    }
    match runtime_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => Ok(PathBuf::from(dir).join(display)),
        None => Err(GateError::Unreachable {
            socket: display,
            source: io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"),
        }),
    }
}

fn wayland_socket_inherited() -> bool {
    env::var_os("WAYLAND_SOCKET").is_some_and(|fd| !fd.is_empty())
}

fn display() -> Result<std::ffi::OsString, GateError> {
    env::var_os("WAYLAND_DISPLAY")
        .filter(|display| !display.is_empty())
        .ok_or(GateError::Unset)
}

/// Cheap check for the forward path: a Wayland session is advertised.
pub fn check_env() -> Result<(), GateError> {
    if wayland_socket_inherited() {
        return Ok(());
    }
    display().map(drop)
}

/// Full check before becoming (or spawning) the daemon: the compositor socket accepts connections.
pub fn check_connection() -> Result<(), GateError> {
    if wayland_socket_inherited() {
        return Ok(());
    }
    let socket = socket_path(&display()?, env::var_os("XDG_RUNTIME_DIR").as_deref())?;
    UnixStream::connect(&socket)
        .map(drop)
        .map_err(|source| GateError::Unreachable { socket, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_display_uses_runtime_dir() {
        let path =
            socket_path(OsStr::new("wayland-1"), Some(OsStr::new("/run/user/1000"))).expect("path");
        assert_eq!(path, PathBuf::from("/run/user/1000/wayland-1"));
    }

    #[test]
    fn absolute_display_is_used_as_is() {
        let path = socket_path(OsStr::new("/tmp/wl/sock"), None).expect("path");
        assert_eq!(path, PathBuf::from("/tmp/wl/sock"));
    }

    #[test]
    fn relative_display_without_runtime_dir_fails() {
        let err = socket_path(OsStr::new("wayland-0"), None).expect_err("no runtime dir");
        assert!(err.to_string().contains("wayland-0"), "{err}");
    }

    #[test]
    fn messages_are_clear() {
        assert!(
            GateError::Unset
                .to_string()
                .contains("requires a Wayland session")
        );
        let err = GateError::Unreachable {
            socket: PathBuf::from("/run/user/1/wayland-9"),
            source: io::Error::from(io::ErrorKind::NotFound),
        };
        assert!(err.to_string().contains("/run/user/1/wayland-9"), "{err}");
    }
}
