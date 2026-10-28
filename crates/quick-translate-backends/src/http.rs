//! The shared HTTP client, request helpers and connection warm-up.

use std::error::Error as _;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{Error, Result};

/// The `reqwest` client shared by all backends (rustls, HTTP/2, keep-alive).
pub type HttpClient = reqwest::Client;

/// TCP and TLS connect timeout used by [`build_client`].
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default total request timeout, including streaming the response body.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

const WARM_UP_TIMEOUT: Duration = Duration::from_secs(5);

/// Builds the shared client with a 5 s connect timeout and the given total timeout.
///
/// Backends also apply their own per-request timeout, which takes precedence, so a changed
/// timeout setting does not require a new client. Redirects are never followed, so request
/// bodies cannot be forwarded to another host.
pub fn build_client(timeout: Duration) -> Result<HttpClient> {
    build_client_with(CONNECT_TIMEOUT, timeout)
}

/// Like [`build_client`] with an explicit connect timeout.
pub fn build_client_with(connect_timeout: Duration, timeout: Duration) -> Result<HttpClient> {
    reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("quick-translate/", env!("CARGO_PKG_VERSION")))
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(30))
        .build()
        .map_err(map_error)
}

/// Opens a connection to the origin of `url` with a `HEAD /` request and returns it to the
/// pool. Errors are ignored.
pub async fn warm_up(client: &HttpClient, url: &Url) {
    let mut origin = url.clone();
    origin.set_path("/");
    origin.set_query(None);
    origin.set_fragment(None);
    let host = origin.host_str().unwrap_or_default().to_owned();
    match client.head(origin).timeout(WARM_UP_TIMEOUT).send().await {
        Ok(resp) => tracing::debug!(host, status = resp.status().as_u16(), "warm-up done"),
        Err(e) => tracing::debug!(host, timeout = e.is_timeout(), "warm-up failed"),
    }
}

/// Sends a request, aborting it when `cancel` fires.
pub(crate) async fn send(
    req: reqwest::RequestBuilder,
    cancel: &CancellationToken,
) -> Result<reqwest::Response> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(Error::Cancelled),
        resp = req.send() => resp.map_err(map_error),
    }
}

/// Reads the full body, aborting when `cancel` fires.
pub(crate) async fn body_bytes(
    resp: reqwest::Response,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(Error::Cancelled),
        body = resp.bytes() => body.map(|b| b.to_vec()).map_err(map_error),
    }
}

/// Sleeps for `delay` unless `cancel` fires first.
pub(crate) async fn sleep(delay: Duration, cancel: &CancellationToken) -> Result<()> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(Error::Cancelled),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

/// Maps a transport error to [`Error::Timeout`] or [`Error::Connection`] without the URL.
pub(crate) fn map_error(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        return Error::Timeout;
    }
    let what = if e.is_connect() {
        "could not connect"
    } else if e.is_body() || e.is_decode() {
        "the response was interrupted"
    } else if e.is_builder() {
        "invalid request"
    } else {
        "request failed"
    };
    let mut root = e.source();
    while let Some(next) = root.and_then(|r| r.source()) {
        root = Some(next);
    }
    match root.map(|r| r.to_string()).filter(|r| !r.is_empty()) {
        Some(root) => Error::Connection(format!("{what}: {root}")),
        None => Error::Connection(what.to_owned()),
    }
}

/// Extracts a human-readable message from an error response body.
pub(crate) fn error_message(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let msg = match &value {
        serde_json::Value::Object(map) => match map.get("error") {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Object(err)) => err
                .get("message")
                .and_then(|m| m.as_str())
                .map(str::to_owned),
            _ => map
                .get("message")
                .and_then(|m| m.as_str())
                .map(str::to_owned),
        },
        _ => None,
    }?;
    let msg = msg.trim();
    (!msg.is_empty()).then(|| truncate(msg, 300))
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_message_shapes() {
        assert_eq!(
            error_message(br#"{"error":{"message":"The model `x` does not exist","code":null}}"#)
                .as_deref(),
            Some("The model `x` does not exist")
        );
        assert_eq!(
            error_message(br#"{"error":"model 'x' not found"}"#).as_deref(),
            Some("model 'x' not found")
        );
        assert_eq!(
            error_message(br#"{"message":"Value for 'target_lang' not supported."}"#).as_deref(),
            Some("Value for 'target_lang' not supported.")
        );
        assert_eq!(error_message(b"<html>nope</html>"), None);
        assert_eq!(error_message(br#"{"error":{"message":"  "}}"#), None);
    }

    #[test]
    fn truncates_long_messages() {
        let long = "é".repeat(400);
        let out = truncate(&long, 300);
        assert_eq!(out.chars().count(), 301);
        assert!(out.ends_with('…'));
    }
}
