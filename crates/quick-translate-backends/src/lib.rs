//! DeepL and OpenAI-compatible translation backends behind one streaming trait (design D7).

use std::pin::Pin;

use futures::Stream;
use tokio_util::sync::CancellationToken;

pub mod deepl;
pub mod http;
pub mod openai;
pub mod sse;
pub mod transport;

pub use deepl::{DeeplBackend, DeeplEndpoints, DeeplHost, DeeplOptions, Usage};
pub use http::{CONNECT_TIMEOUT, DEFAULT_TIMEOUT, HttpClient, build_client, warm_up};
pub use openai::{OpenAiBackend, OpenAiOptions};
pub use transport::{EndpointError, check_endpoint, is_loopback};

/// What to translate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub text: String,
    /// DeepL source code (`EN`, `DE`, ...). `None` means auto-detect.
    pub source_lang: Option<String>,
    /// DeepL target code (`EN-US`, `TR`, ...).
    pub target_lang: String,
}

/// One piece of a streamed translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    /// Text to append to the translation pane.
    Delta(String),
    /// Source language detected by the backend (DeepL code, e.g. `DE`).
    Detected(String),
}

/// User-facing failure categories. `Display` is the message shown in the translation pane.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("The translation backend is not configured")]
    NotConfigured,
    #[error("Invalid API key")]
    InvalidKey,
    #[error("DeepL quota exhausted")]
    QuotaExceeded,
    #[error("Rate limited by the translation service, try again shortly")]
    RateLimited,
    #[error("Connection error: {0}")]
    Connection(String),
    #[error("The request timed out")]
    Timeout,
    #[error("Insecure endpoint: plain http is only allowed for loopback hosts unless confirmed")]
    InsecureEndpoint,
    /// An error message reported by the endpoint itself (e.g. unknown model).
    #[error("{0}")]
    Service(String),
    #[error("Cancelled")]
    Cancelled,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub type ChunkStream = Pin<Box<dyn Stream<Item = Result<Chunk>> + Send>>;

/// A translation backend. Dropping the stream or cancelling the token aborts the HTTP request.
pub trait Translator: Send + Sync {
    fn translate(
        &self,
        req: Request,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<ChunkStream>> + Send + '_>>;

    /// Opens a connection to the backend host so the next request skips TCP and TLS setup.
    /// Failures are ignored. The daemon calls this in parallel with selection capture.
    fn warm_up(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}
