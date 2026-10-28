//! Shared helpers: a raw HTTP/1.1 server for cases wiremock cannot express (progressive
//! chunked bodies, abrupt disconnects, observing client-side aborts, counting connections).

#![allow(dead_code, clippy::unwrap_used)]

use std::net::SocketAddr;
use std::time::Duration;

use futures::StreamExt;
use quick_translate_backends::{Chunk, ChunkStream, Error, HttpClient, Request};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub fn client() -> HttpClient {
    quick_translate_backends::build_client(Duration::from_secs(10)).unwrap()
}

pub fn request(text: &str, source: Option<&str>, target: &str) -> Request {
    Request {
        text: text.into(),
        source_lang: source.map(Into::into),
        target_lang: target.into(),
    }
}

/// Collects a stream into its chunks and the first error, if any.
pub async fn collect(mut stream: ChunkStream) -> (Vec<Chunk>, Option<Error>) {
    let mut chunks = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(c) => chunks.push(c),
            Err(e) => return (chunks, Some(e)),
        }
    }
    (chunks, None)
}

pub fn text_of(chunks: &[Chunk]) -> String {
    chunks
        .iter()
        .filter_map(|c| match c {
            Chunk::Delta(d) => Some(d.as_str()),
            Chunk::Detected(_) => None,
        })
        .collect()
}

pub async fn listener() -> (TcpListener, SocketAddr) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    (l, addr)
}

/// Reads one request (head and `Content-Length` body). `None` on EOF.
pub async fn read_request(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
    let len = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

pub const SSE_HEAD: &str =
    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";

pub async fn write_chunk(stream: &mut TcpStream, data: &str) {
    let frame = format!("{:x}\r\n{data}\r\n", data.len());
    stream.write_all(frame.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
}

pub fn sse_delta(text: &str) -> String {
    let json = serde_json::json!({"choices": [{"index": 0, "delta": {"content": text}}]});
    format!("data: {json}\n\n")
}

/// Waits until the peer closes the connection. Returns `true` if it did within `limit`.
pub async fn peer_closes(stream: &mut TcpStream, limit: Duration) -> bool {
    let mut tmp = [0u8; 1024];
    tokio::time::timeout(limit, async {
        loop {
            match stream.read(&mut tmp).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    })
    .await
    .is_ok()
}
