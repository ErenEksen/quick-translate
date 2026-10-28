//! A minimal blocking D-Bus client for the CLI forward path: one connection, SASL `EXTERNAL`,
//! `Hello`, one method call. Messages are marshalled with zbus/zvariant (which need no runtime);
//! the transport is a plain `std` socket, so no Tokio runtime or extra thread is ever started.

use std::ffi::OsString;
use std::io::{self, ErrorKind, Read, Write};
use std::num::NonZeroU32;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use zbus::Message;
use zbus::message::Flags;
use zbus::zvariant::serialized::{Context, Data};
use zbus::zvariant::{DynamicType, Endian, OwnedValue, Type, Value};

/// D-Bus caps messages at 128 MiB.
const MAX_MESSAGE: usize = 128 * 1024 * 1024;
const HELLO_SERIAL: u32 = 1;
const CALL_SERIAL: u32 = 2;

const TYPE_METHOD_RETURN: u8 = 2;
const TYPE_ERROR: u8 = 3;
const FIELD_ERROR_NAME: u8 = 4;
const FIELD_REPLY_SERIAL: u8 = 5;

#[derive(Debug)]
pub(crate) enum WireError {
    /// No bus could be reached at the given address.
    Unavailable(io::Error),
    Io(io::Error),
    Protocol(String),
    Remote {
        name: String,
        message: String,
    },
}

impl From<zbus::Error> for WireError {
    fn from(err: zbus::Error) -> Self {
        Self::Protocol(err.to_string())
    }
}

impl From<zbus::zvariant::Error> for WireError {
    fn from(err: zbus::zvariant::Error) -> Self {
        Self::Protocol(err.to_string())
    }
}

/// A method call to send.
pub(crate) struct Call<'a> {
    pub destination: &'a str,
    pub path: &'a str,
    pub interface: &'a str,
    pub member: &'a str,
}

impl Call<'_> {
    fn build<B>(&self, body: &B) -> zbus::Result<Message>
    where
        B: serde::Serialize + DynamicType,
    {
        Message::method_call(self.path, self.member)?
            .destination(self.destination)?
            .interface(self.interface)?
            .with_flags(Flags::NoAutoStart)?
            .serial(serial(CALL_SERIAL))
            .build(body)
    }
}

fn serial(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap_or(NonZeroU32::MIN)
}

/// A method return received for our call.
pub(crate) struct Returned {
    endian: Endian,
    body: Vec<u8>,
}

impl Returned {
    pub fn body<T>(&self) -> Result<T, WireError>
    where
        T: for<'de> Deserialize<'de> + Type,
    {
        let data = Data::new(&self.body[..], Context::new_dbus(self.endian, 0));
        Ok(data.deserialize::<T>()?.0)
    }
}

/// Connects to `address`, authenticates and performs one method call.
pub(crate) fn call<B>(
    address: &str,
    timeout: Duration,
    call: &Call<'_>,
    body: &B,
) -> Result<Returned, WireError>
where
    B: serde::Serialize + DynamicType,
{
    let stream = connect(address).map_err(WireError::Unavailable)?;
    let mut conn = Conn {
        stream,
        buf: Vec::with_capacity(512),
    };
    conn.stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| conn.stream.set_write_timeout(Some(timeout)))
        .map_err(WireError::Io)?;
    conn.authenticate()?;

    let hello = Message::method_call("/org/freedesktop/DBus", "Hello")?
        .destination("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .serial(serial(HELLO_SERIAL))
        .build(&())?;
    let call = call.build(body)?;
    let mut out = Vec::with_capacity(512);
    out.extend_from_slice(b"BEGIN\r\n");
    out.extend_from_slice(hello.data().bytes());
    out.extend_from_slice(call.data().bytes());
    conn.stream.write_all(&out).map_err(WireError::Io)?;

    loop {
        let message = conn.read_message().map_err(WireError::Io)?;
        let incoming = parse(&message)?;
        if incoming.reply_serial != Some(CALL_SERIAL) {
            if incoming.reply_serial == Some(HELLO_SERIAL) && incoming.kind == TYPE_ERROR {
                return Err(remote_error(&incoming, &message));
            }
            continue;
        }
        return match incoming.kind {
            TYPE_METHOD_RETURN => Ok(Returned {
                endian: incoming.endian,
                body: message[incoming.body_start..].to_vec(),
            }),
            TYPE_ERROR => Err(remote_error(&incoming, &message)),
            other => Err(WireError::Protocol(format!(
                "unexpected message type {other} in reply"
            ))),
        };
    }
}

fn remote_error(incoming: &Incoming, message: &[u8]) -> WireError {
    let data = Data::new(
        &message[incoming.body_start..],
        Context::new_dbus(incoming.endian, 0),
    );
    WireError::Remote {
        name: incoming.error_name.clone().unwrap_or_default(),
        message: data
            .deserialize::<String>()
            .map(|(text, _)| text)
            .unwrap_or_default(),
    }
}

struct Conn {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl Conn {
    fn authenticate(&mut self) -> Result<(), WireError> {
        let uid = rustix::process::getuid().as_raw().to_string();
        let hex: String = uid.bytes().map(|b| format!("{b:02x}")).collect();
        self.stream
            .write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())
            .map_err(WireError::Io)?;
        let line = self.read_line().map_err(WireError::Io)?;
        if line.starts_with("OK ") {
            Ok(())
        } else {
            Err(WireError::Protocol(format!(
                "bus rejected authentication: {line}"
            )))
        }
    }

    fn fill(&mut self) -> io::Result<()> {
        let mut chunk = [0u8; 4096];
        let n = self.stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::new(
                ErrorKind::UnexpectedEof,
                "bus closed the connection",
            ));
        }
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(())
    }

    fn read_line(&mut self) -> io::Result<String> {
        loop {
            if let Some(end) = self.buf.windows(2).position(|w| w == b"\r\n") {
                let line = String::from_utf8_lossy(&self.buf[..end]).into_owned();
                self.buf.drain(..end + 2);
                return Ok(line);
            }
            if self.buf.len() > 16 * 1024 {
                return Err(io::Error::new(ErrorKind::InvalidData, "SASL line too long"));
            }
            self.fill()?;
        }
    }

    fn read_message(&mut self) -> io::Result<Vec<u8>> {
        while self.buf.len() < 16 {
            self.fill()?;
        }
        let read_u32 = |bytes: &[u8], at: usize| {
            let raw = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
            if bytes[0] == b'B' {
                u32::from_be_bytes(raw)
            } else {
                u32::from_le_bytes(raw)
            }
        };
        if !matches!(self.buf[0], b'l' | b'B') {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "bad endianness byte",
            ));
        }
        let body_len = read_u32(&self.buf, 4) as usize;
        let fields_len = read_u32(&self.buf, 12) as usize;
        let total = align8(16 + fields_len) + body_len;
        if total > MAX_MESSAGE {
            return Err(io::Error::new(ErrorKind::InvalidData, "message too large"));
        }
        while self.buf.len() < total {
            self.fill()?;
        }
        Ok(self.buf.drain(..total).collect())
    }
}

struct Incoming {
    endian: Endian,
    kind: u8,
    reply_serial: Option<u32>,
    error_name: Option<String>,
    body_start: usize,
}

type RawHeader = (u8, u8, u8, u8, u32, u32, Vec<(u8, OwnedValue)>);

fn parse(message: &[u8]) -> Result<Incoming, WireError> {
    let endian = if message[0] == b'B' {
        Endian::Big
    } else {
        Endian::Little
    };
    let data = Data::new(message, Context::new_dbus(endian, 0));
    let ((_, kind, _, _, _, _, fields), header_len) = data.deserialize::<RawHeader>()?;
    let mut incoming = Incoming {
        endian,
        kind,
        reply_serial: None,
        error_name: None,
        body_start: align8(header_len),
    };
    for (code, value) in fields {
        match (code, &*value) {
            (FIELD_REPLY_SERIAL, Value::U32(serial)) => incoming.reply_serial = Some(*serial),
            (FIELD_ERROR_NAME, Value::Str(name)) => incoming.error_name = Some(name.to_string()),
            _ => {}
        }
    }
    if incoming.body_start > message.len() {
        return Err(WireError::Protocol("truncated message".into()));
    }
    Ok(incoming)
}

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// Connects to the first reachable `unix:` address in a `;`-separated D-Bus address list.
fn connect(address: &str) -> io::Result<UnixStream> {
    let mut last = io::Error::new(
        ErrorKind::InvalidInput,
        format!("no supported unix transport in D-Bus address {address:?}"),
    );
    for entry in address.split(';').filter(|entry| !entry.is_empty()) {
        let Some(params) = entry.strip_prefix("unix:") else {
            continue;
        };
        for param in params.split(',') {
            let result = match param.split_once('=') {
                Some(("path", value)) => unescape(value)
                    .and_then(|path| UnixStream::connect(PathBuf::from(OsString::from_vec(path)))),
                Some(("abstract", value)) => unescape(value)
                    .and_then(SocketAddr::from_abstract_name)
                    .and_then(|addr| UnixStream::connect_addr(&addr)),
                _ => continue,
            };
            match result {
                Ok(stream) => return Ok(stream),
                Err(err) => last = err,
            }
        }
    }
    Err(last)
}

fn unescape(value: &str) -> io::Result<Vec<u8>> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or_else(|| {
                    io::Error::new(ErrorKind::InvalidInput, "bad %-escape in D-Bus address")
                })?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

/// Escapes a path for use as a `unix:path=` value.
pub(crate) fn escape(value: &[u8]) -> String {
    let mut out = String::with_capacity(value.len());
    for &b in value {
        if b.is_ascii_alphanumeric() || b"-_/.\\*".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02x}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_escaping_round_trips() {
        let raw = b"/run/user/1000/my bus,x=y";
        let escaped = escape(raw);
        assert!(!escaped.contains([' ', ',', '=']));
        assert_eq!(unescape(&escaped).expect("unescape"), raw);
        assert!(unescape("%zz").is_err());
    }

    #[test]
    fn unsupported_addresses_fail() {
        assert!(connect("tcp:host=localhost,port=1").is_err());
        assert!(connect("").is_err());
    }

    #[test]
    fn parses_a_method_return() {
        let call = Message::method_call("/a", "B")
            .and_then(|b| b.serial(serial(9)).build(&()))
            .expect("call");
        let reply = Message::method_return(&call.header())
            .and_then(|b| b.serial(serial(3)).build(&("hi",)))
            .expect("reply");
        let bytes = reply.data().bytes();
        let incoming = parse(bytes).expect("parse");
        assert_eq!(incoming.kind, TYPE_METHOD_RETURN);
        assert_eq!(incoming.reply_serial, Some(9));
        let returned = Returned {
            endian: incoming.endian,
            body: bytes[incoming.body_start..].to_vec(),
        };
        assert_eq!(returned.body::<String>().expect("body"), "hi");
    }

    #[test]
    fn parses_an_error() {
        let call = Message::method_call("/a", "B")
            .and_then(|b| b.serial(serial(5)).build(&()))
            .expect("call");
        let reply = Message::error(&call.header(), "org.example.Error.Nope")
            .and_then(|b| b.build(&("no way",)))
            .expect("error");
        let bytes = reply.data().bytes();
        let incoming = parse(bytes).expect("parse");
        assert_eq!(incoming.kind, TYPE_ERROR);
        match remote_error(&incoming, bytes) {
            WireError::Remote { name, message } => {
                assert_eq!(name, "org.example.Error.Nope");
                assert_eq!(message, "no way");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
