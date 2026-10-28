//! Minimal Sway IPC (the i3-ipc wire format) for one request: the zero-length cursor move that
//! makes Sway send `wl_pointer.enter` to a freshly mapped surface under a resting pointer
//! (spike 2.1). It moves the cursor by 0 px and sends no key, text or button input.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAGIC: &[u8; 6] = b"i3-ipc";
const RUN_COMMAND: u32 = 0;
const MAX_REPLY: u32 = 1 << 16;

/// Re-sends the pointer position to the surface under the cursor without moving it.
pub const CURSOR_REBASE: &str = "seat - cursor move 0 0";

/// The Sway IPC socket from `SWAYSOCK`.
pub fn socket_from_env() -> Option<PathBuf> {
    std::env::var_os("SWAYSOCK")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn frame(kind: u32, payload: &str) -> Vec<u8> {
    let mut message = Vec::with_capacity(14 + payload.len());
    message.extend_from_slice(MAGIC);
    message.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    message.extend_from_slice(&kind.to_ne_bytes());
    message.extend_from_slice(payload.as_bytes());
    message
}

/// Sends `RUN_COMMAND` and returns Sway's JSON reply. Fails when the reply reports no success.
pub fn run_command(socket: &Path, command: &str, timeout: Duration) -> io::Result<String> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(&frame(RUN_COMMAND, command))?;
    let mut header = [0u8; 14];
    stream.read_exact(&mut header)?;
    if &header[..6] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not an i3-ipc reply",
        ));
    }
    let len = u32::from_ne_bytes([header[6], header[7], header[8], header[9]]);
    let kind = u32::from_ne_bytes([header[10], header[11], header[12], header[13]]);
    if kind != RUN_COMMAND || len > MAX_REPLY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected i3-ipc reply",
        ));
    }
    let mut body = vec![0u8; len as usize];
    stream.read_exact(&mut body)?;
    let reply = String::from_utf8_lossy(&body).into_owned();
    if reply.replace(' ', "").contains("\"success\":true") {
        Ok(reply)
    } else {
        Err(io::Error::other(format!(
            "sway refused the command: {reply}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;

    use super::*;

    fn fake_sway(
        reply: &'static str,
    ) -> (tempfile::TempDir, PathBuf, std::thread::JoinHandle<Vec<u8>>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sway-ipc.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut header = [0u8; 14];
            conn.read_exact(&mut header).expect("header");
            let len = u32::from_ne_bytes([header[6], header[7], header[8], header[9]]) as usize;
            let mut payload = vec![0u8; len];
            conn.read_exact(&mut payload).expect("payload");
            conn.write_all(&frame(RUN_COMMAND, reply)).expect("reply");
            [header.to_vec(), payload].concat()
        });
        (dir, path, server)
    }

    #[test]
    fn sends_one_run_command() {
        let (_dir, path, server) = fake_sway(r#"[ { "success": true } ]"#);
        run_command(&path, CURSOR_REBASE, Duration::from_secs(2)).expect("success");
        let request = server.join().expect("server");
        assert_eq!(&request[..6], MAGIC);
        assert_eq!(&request[10..14], &RUN_COMMAND.to_ne_bytes());
        assert_eq!(&request[14..], CURSOR_REBASE.as_bytes());
    }

    #[test]
    fn reports_a_refused_command() {
        let (_dir, path, server) = fake_sway(r#"[{"success":false,"error":"no seat"}]"#);
        let err = run_command(&path, CURSOR_REBASE, Duration::from_secs(2)).expect_err("refused");
        assert!(err.to_string().contains("no seat"), "{err}");
        server.join().expect("server");
    }

    #[test]
    fn missing_socket_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            run_command(
                &dir.path().join("none"),
                CURSOR_REBASE,
                Duration::from_millis(100)
            )
            .is_err()
        );
    }
}
