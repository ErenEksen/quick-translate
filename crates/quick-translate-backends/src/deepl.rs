//! DeepL API v2 backend: one `POST /v2/translate` per request, `GET /v2/usage` for quota.

use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::http::{self, HttpClient};
use crate::{Chunk, ChunkStream, DEFAULT_TIMEOUT, Error, Request, Result, Translator};

/// Retries after HTTP 429 before giving up with [`Error::RateLimited`].
pub const MAX_RATE_LIMIT_RETRIES: u32 = 3;

/// One of the two DeepL API hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeeplHost {
    /// `api-free.deepl.com`, for keys ending in `:fx`.
    Free,
    /// `api.deepl.com`.
    Pro,
}

impl DeeplHost {
    /// The host name, as stored in config `deepl.cached-host`.
    pub const fn hostname(self) -> &'static str {
        match self {
            Self::Free => "api-free.deepl.com",
            Self::Pro => "api.deepl.com",
        }
    }

    /// Parses a host name written by [`Self::hostname`].
    pub fn from_hostname(name: &str) -> Option<Self> {
        match name.trim() {
            "api-free.deepl.com" => Some(Self::Free),
            "api.deepl.com" => Some(Self::Pro),
            _ => None,
        }
    }

    /// The host implied by the key: `:fx` keys belong to the free API.
    pub fn for_key(key: &str) -> Self {
        if key.trim().ends_with(":fx") {
            Self::Free
        } else {
            Self::Pro
        }
    }

    /// The other host.
    pub const fn other(self) -> Self {
        match self {
            Self::Free => Self::Pro,
            Self::Pro => Self::Free,
        }
    }
}

/// Base URLs for the two hosts. Tests point these at a mock server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeeplEndpoints {
    pub free: Url,
    pub pro: Url,
}

impl Default for DeeplEndpoints {
    fn default() -> Self {
        #[allow(clippy::unwrap_used)]
        Self {
            free: Url::parse("https://api-free.deepl.com").unwrap(),
            pro: Url::parse("https://api.deepl.com").unwrap(),
        }
    }
}

impl DeeplEndpoints {
    fn get(&self, host: DeeplHost) -> &Url {
        match host {
            DeeplHost::Free => &self.free,
            DeeplHost::Pro => &self.pro,
        }
    }
}

/// Construction parameters for [`DeeplBackend`].
#[derive(Clone, PartialEq, Eq)]
pub struct DeeplOptions {
    /// The API key. Empty means not configured.
    pub api_key: String,
    /// The host that last worked for this key (config `deepl.cached-host`). `None` derives it
    /// from the key suffix.
    pub cached_host: Option<DeeplHost>,
    /// Total timeout per HTTP request.
    pub timeout: Duration,
    pub endpoints: DeeplEndpoints,
    /// First 429 backoff delay. Doubles on each retry.
    pub retry_base_delay: Duration,
}

impl DeeplOptions {
    /// Production defaults for `api_key`.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            cached_host: None,
            timeout: DEFAULT_TIMEOUT,
            endpoints: DeeplEndpoints::default(),
            retry_base_delay: Duration::from_millis(500),
        }
    }
}

impl std::fmt::Debug for DeeplOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeeplOptions")
            .field(
                "api_key",
                &if self.api_key.is_empty() {
                    "<empty>"
                } else {
                    "<redacted>"
                },
            )
            .field("cached_host", &self.cached_host)
            .field("timeout", &self.timeout)
            .field("endpoints", &self.endpoints)
            .field("retry_base_delay", &self.retry_base_delay)
            .finish()
    }
}

/// Character usage reported by `GET /v2/usage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Usage {
    pub character_count: u64,
    pub character_limit: u64,
}

type HostListener = Arc<dyn Fn(DeeplHost) + Send + Sync>;

/// The DeepL backend. Emits `Detected` (when DeepL reports it) followed by one `Delta`.
pub struct DeeplBackend {
    client: HttpClient,
    api_key: String,
    timeout: Duration,
    endpoints: DeeplEndpoints,
    retry_base_delay: Duration,
    host: Mutex<DeeplHost>,
    on_host_change: Option<HostListener>,
}

impl std::fmt::Debug for DeeplBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeeplBackend")
            .field("host", &self.host())
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct TranslateBody<'a> {
    text: [&'a str; 1],
    target_lang: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_lang: Option<&'a str>,
}

#[derive(Deserialize)]
struct TranslateResponse {
    translations: Vec<Translation>,
}

#[derive(Deserialize)]
struct Translation {
    text: String,
    #[serde(default)]
    detected_source_language: Option<String>,
}

impl DeeplBackend {
    pub fn new(client: HttpClient, options: DeeplOptions) -> Self {
        let host = options
            .cached_host
            .unwrap_or_else(|| DeeplHost::for_key(&options.api_key));
        Self {
            client,
            api_key: options.api_key.trim().to_owned(),
            timeout: options.timeout,
            endpoints: options.endpoints,
            retry_base_delay: options.retry_base_delay,
            host: Mutex::new(host),
            on_host_change: None,
        }
    }

    /// Calls `f` with the new host whenever a 403 fallback switches hosts, so the daemon can
    /// persist it to config `deepl.cached-host`.
    #[must_use]
    pub fn on_host_change(mut self, f: impl Fn(DeeplHost) + Send + Sync + 'static) -> Self {
        self.on_host_change = Some(Arc::new(f));
        self
    }

    /// The host the next request goes to.
    pub fn host(&self) -> DeeplHost {
        *self.host.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Validates the key and fetches the quota with `GET /v2/usage`.
    pub async fn usage(&self) -> Result<Usage> {
        let cancel = CancellationToken::new();
        let resp = self.execute("/v2/usage", None, &cancel).await?;
        let body = http::body_bytes(resp, &cancel).await?;
        serde_json::from_slice(&body).map_err(|_| unexpected())
    }

    /// Opens a connection to the current host.
    pub async fn warm_up(&self) {
        http::warm_up(&self.client, self.endpoints.get(self.host())).await;
    }

    async fn translate_inner(
        &self,
        req: Request,
        cancel: CancellationToken,
    ) -> Result<ChunkStream> {
        let body = TranslateBody {
            text: [&req.text],
            target_lang: &req.target_lang,
            source_lang: req.source_lang.as_deref().filter(|s| !s.is_empty()),
        };
        let body = serde_json::to_vec(&body).map_err(|_| unexpected())?;
        let resp = self.execute("/v2/translate", Some(body), &cancel).await?;
        let body = http::body_bytes(resp, &cancel).await?;
        let parsed: TranslateResponse = serde_json::from_slice(&body).map_err(|_| unexpected())?;
        let first = parsed
            .translations
            .into_iter()
            .next()
            .ok_or_else(unexpected)?;

        let mut chunks = Vec::with_capacity(2);
        if let Some(lang) = first.detected_source_language.filter(|l| !l.is_empty()) {
            chunks.push(Ok(Chunk::Detected(lang.to_ascii_uppercase())));
        }
        chunks.push(Ok(Chunk::Delta(first.text)));
        Ok(Box::pin(futures::stream::iter(chunks)))
    }

    /// Runs a request with host fallback on 403 and exponential backoff on 429.
    async fn execute(
        &self,
        path: &str,
        body: Option<Vec<u8>>,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response> {
        if self.api_key.is_empty() {
            return Err(Error::NotConfigured);
        }
        let start = self.host();
        let mut host = start;
        let mut switched = false;
        let mut retries = 0;
        loop {
            let url = self
                .endpoints
                .get(host)
                .join(path)
                .map_err(|_| Error::NotConfigured)?;
            let req = match &body {
                Some(body) => self
                    .client
                    .post(url)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body.clone()),
                None => self.client.get(url),
            }
            .header(
                reqwest::header::AUTHORIZATION,
                format!("DeepL-Auth-Key {}", self.api_key),
            )
            .timeout(self.timeout);

            let resp = http::send(req, cancel).await?;
            let status = resp.status().as_u16();
            tracing::debug!(host = host.hostname(), path, status, "DeepL response");
            match status {
                200..=299 => {
                    if host != start {
                        self.set_host(host);
                    }
                    return Ok(resp);
                }
                403 if !switched => {
                    switched = true;
                    host = host.other();
                }
                401 | 403 => return Err(Error::InvalidKey),
                456 => return Err(Error::QuotaExceeded),
                429 | 529 if retries < MAX_RATE_LIMIT_RETRIES => {
                    let delay = self.retry_base_delay * 2u32.pow(retries);
                    retries += 1;
                    tracing::debug!(retry = retries, ?delay, "DeepL rate limited, backing off");
                    http::sleep(delay, cancel).await?;
                }
                429 | 529 => return Err(Error::RateLimited),
                _ => {
                    let body = http::body_bytes(resp, cancel).await.unwrap_or_default();
                    return Err(Error::Service(match http::error_message(&body) {
                        Some(msg) => format!("DeepL: {msg}"),
                        None => format!("DeepL returned HTTP {status}"),
                    }));
                }
            }
        }
    }

    fn set_host(&self, host: DeeplHost) {
        *self.host.lock().unwrap_or_else(PoisonError::into_inner) = host;
        tracing::debug!(host = host.hostname(), "DeepL host switched");
        if let Some(f) = &self.on_host_change {
            f(host);
        }
    }
}

fn unexpected() -> Error {
    Error::Service("Unexpected response from DeepL".into())
}

impl Translator for DeeplBackend {
    fn translate(
        &self,
        req: Request,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<ChunkStream>> + Send + '_>> {
        Box::pin(self.translate_inner(req, cancel))
    }

    fn warm_up(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(DeeplBackend::warm_up(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_for_key() {
        assert_eq!(DeeplHost::for_key("abc-123:fx"), DeeplHost::Free);
        assert_eq!(DeeplHost::for_key("abc-123:fx\n"), DeeplHost::Free);
        assert_eq!(DeeplHost::for_key("abc-123"), DeeplHost::Pro);
        assert_eq!(DeeplHost::for_key("abc:fxx"), DeeplHost::Pro);
    }

    #[test]
    fn hostname_round_trip() {
        for h in [DeeplHost::Free, DeeplHost::Pro] {
            assert_eq!(DeeplHost::from_hostname(h.hostname()), Some(h));
            assert_eq!(h.other().other(), h);
        }
        assert_eq!(DeeplHost::from_hostname("example.com"), None);
    }

    #[test]
    fn cached_host_wins_over_suffix() {
        let client = reqwest::Client::new();
        let mut opts = DeeplOptions::new("k:fx");
        assert_eq!(
            DeeplBackend::new(client.clone(), opts.clone()).host(),
            DeeplHost::Free
        );
        opts.cached_host = Some(DeeplHost::Pro);
        assert_eq!(DeeplBackend::new(client, opts).host(), DeeplHost::Pro);
    }

    #[test]
    fn debug_redacts_key() {
        let dbg = format!("{:?}", DeeplOptions::new("secret-key:fx"));
        assert!(!dbg.contains("secret-key"));
    }

    #[test]
    fn body_omits_source_lang_for_auto() {
        let auto = TranslateBody {
            text: ["hi"],
            target_lang: "DE",
            source_lang: None,
        };
        assert_eq!(
            serde_json::to_value(&auto).unwrap(),
            serde_json::json!({"text": ["hi"], "target_lang": "DE"})
        );
    }
}
