//! Selection capture pipeline (design D5, spec selection-capture).
//!
//! [`capture`] tries each [`SelectionSource`] in order (data-control PRIMARY, focus-time PRIMARY
//! from the toolkit, CLIPBOARD) with a per-step timeout and returns the first one holding text.
//! Sources only ever read; nothing here creates a data source or sets a selection.
//!
//! Captured text is never logged: logs carry origins, MIME types and lengths only.

use std::fmt;
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use quick_translate_core::MAX_SOURCE_CHARS;

use crate::wayland::{DataControlHandle, Selection};

/// Budget for one capture step (spec "Capture time budget").
pub const STEP_TIMEOUT: Duration = Duration::from_millis(150);

/// Accepted MIME types, most preferred first. Anything else is ignored.
pub const TEXT_MIME_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];

/// Pick the most preferred accepted text type from an offer's MIME types.
///
/// Returns the offered string itself (so it can be passed back verbatim to `receive`). The
/// comparison ignores ASCII case and spaces around `;`.
pub fn pick_text_mime(offered: &[String]) -> Option<&str> {
    let canon = |m: &str| m.replace(' ', "").to_ascii_lowercase();
    TEXT_MIME_TYPES.iter().find_map(|want| {
        let want = canon(want);
        offered
            .iter()
            .find(|m| canon(m) == want)
            .map(String::as_str)
    })
}

/// Raw selection content as delivered by a source.
#[derive(Clone, PartialEq, Eq)]
pub struct Payload {
    /// The MIME type that was read, if the source knows it.
    pub mime: Option<String>,
    pub bytes: Vec<u8>,
    /// The source stopped reading early because the content was huge.
    pub capped: bool,
}

impl Payload {
    /// Text already decoded by the toolkit.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            mime: None,
            bytes: text.into().into_bytes(),
            capped: false,
        }
    }

    pub fn bytes(mime: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            mime: Some(mime.into()),
            bytes: bytes.into(),
            capped: false,
        }
    }
}

impl fmt::Debug for Payload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Payload")
            .field("mime", &self.mime)
            .field("len", &self.bytes.len())
            .field("capped", &self.capped)
            .finish()
    }
}

/// Where the captured text came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// PRIMARY read in the background through ext/wlr data-control.
    DataControlPrimary,
    /// PRIMARY read by the toolkit after the popup received keyboard focus.
    FocusPrimary,
    Clipboard,
    /// Nothing usable: the popup opens with an empty, focused source pane.
    Empty,
}

/// Error type of [`SelectionSource::read`]. Must not contain selection content.
pub type SourceError = Box<dyn std::error::Error + Send + Sync>;

/// One capture step. The data-control thread implements it via [`DataControlSource`]; the UI
/// implements it for the focus-time PRIMARY read and the toolkit CLIPBOARD read.
pub trait SelectionSource: Send + Sync {
    fn origin(&self) -> Origin;

    /// Read the selection. `Ok(None)` means empty or no text type offered. Implementations must
    /// only read: never take ownership of or modify PRIMARY or CLIPBOARD. [`capture`] enforces
    /// the time budget, so a read may simply wait (e.g. for keyboard focus).
    fn read(&self) -> BoxFuture<'_, Result<Option<Payload>, SourceError>>;
}

/// Reads PRIMARY or CLIPBOARD through the data-control thread.
#[derive(Debug, Clone)]
pub struct DataControlSource {
    handle: DataControlHandle,
    which: Selection,
    timeout: Duration,
}

impl DataControlSource {
    pub fn primary(handle: DataControlHandle) -> Self {
        Self::new(handle, Selection::Primary)
    }

    pub fn clipboard(handle: DataControlHandle) -> Self {
        Self::new(handle, Selection::Clipboard)
    }

    fn new(handle: DataControlHandle, which: Selection) -> Self {
        Self {
            handle,
            which,
            timeout: STEP_TIMEOUT,
        }
    }

    /// How long the thread waits for the selection owner (defaults to [`STEP_TIMEOUT`]).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl SelectionSource for DataControlSource {
    fn origin(&self) -> Origin {
        match self.which {
            Selection::Primary => Origin::DataControlPrimary,
            Selection::Clipboard => Origin::Clipboard,
        }
    }

    fn read(&self) -> BoxFuture<'_, Result<Option<Payload>, SourceError>> {
        let read = self.handle.read(self.which, self.timeout);
        Box::pin(async move { read.await.map_err(Into::into) })
    }
}

/// The result of a capture.
#[derive(Clone, PartialEq, Eq)]
pub struct Captured {
    /// Trimmed text, at most [`MAX_SOURCE_CHARS`] characters. Empty for [`Origin::Empty`].
    pub text: String,
    /// The text was cut to [`MAX_SOURCE_CHARS`]; the popup shows a notice.
    pub truncated: bool,
    pub origin: Origin,
}

impl Captured {
    pub fn empty() -> Self {
        Self {
            text: String::new(),
            truncated: false,
            origin: Origin::Empty,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

impl fmt::Debug for Captured {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Captured")
            .field("chars", &self.text.chars().count())
            .field("truncated", &self.truncated)
            .field("origin", &self.origin)
            .finish()
    }
}

/// Decode lossily, trim surrounding whitespace (keeping internal line breaks) and truncate to
/// [`MAX_SOURCE_CHARS`] characters. `None` if nothing but whitespace remains.
pub fn normalize(payload: &Payload) -> Option<(String, bool)> {
    let decoded = String::from_utf8_lossy(&payload.bytes);
    let trimmed = decoded.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (text, truncated) = match trimmed.char_indices().nth(MAX_SOURCE_CHARS) {
        Some((cut, _)) => (&trimmed[..cut], true),
        None => (trimmed, false),
    };
    Some((text.to_owned(), truncated || payload.capped))
}

/// Run the capture pipeline: the first source yielding non-blank text wins. Each step is
/// abandoned after `step_timeout`; errors and timeouts fall through to the next source.
pub async fn capture(sources: &[&dyn SelectionSource], step_timeout: Duration) -> Captured {
    let started = Instant::now();
    for source in sources {
        let origin = source.origin();
        let step = Instant::now();
        let payload = match tokio::time::timeout(step_timeout, source.read()).await {
            Ok(Ok(Some(payload))) => payload,
            Ok(Ok(None)) => {
                tracing::debug!(?origin, "capture step: no text");
                continue;
            }
            Ok(Err(e)) => {
                tracing::debug!(?origin, error = %e, "capture step failed");
                continue;
            }
            Err(_) => {
                tracing::debug!(?origin, ?step_timeout, "capture step timed out");
                continue;
            }
        };
        let Some((text, truncated)) = normalize(&payload) else {
            tracing::debug!(
                ?origin,
                bytes = payload.bytes.len(),
                "capture step: blank text"
            );
            continue;
        };
        let captured = Captured {
            text,
            truncated,
            origin,
        };
        tracing::debug!(
            ?origin,
            mime = payload.mime.as_deref(),
            bytes = payload.bytes.len(),
            chars = captured.text.chars().count(),
            truncated,
            step_ms = step.elapsed().as_millis() as u64,
            total_ms = started.elapsed().as_millis() as u64,
            "captured selection"
        );
        return captured;
    }
    tracing::debug!(
        total_ms = started.elapsed().as_millis() as u64,
        "capture: nothing usable"
    );
    Captured::empty()
}

#[cfg(test)]
mod tests;
