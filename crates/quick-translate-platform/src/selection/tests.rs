use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;

enum Behavior {
    Gives(Payload),
    Nothing,
    Fails,
    Hangs,
}

struct Fake {
    origin: Origin,
    behavior: Behavior,
    calls: AtomicUsize,
}

impl Fake {
    fn new(origin: Origin, behavior: Behavior) -> Self {
        Self {
            origin,
            behavior,
            calls: AtomicUsize::new(0),
        }
    }

    fn text(origin: Origin, text: &str) -> Self {
        Self::new(origin, Behavior::Gives(Payload::text(text)))
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl SelectionSource for Fake {
    fn origin(&self) -> Origin {
        self.origin
    }

    fn read(&self) -> BoxFuture<'_, Result<Option<Payload>, SourceError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            match &self.behavior {
                Behavior::Gives(p) => Ok(Some(p.clone())),
                Behavior::Nothing => Ok(None),
                Behavior::Fails => Err("owner vanished".into()),
                Behavior::Hangs => {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    Ok(Some(Payload::text("too late")))
                }
            }
        })
    }
}

const P: Origin = Origin::DataControlPrimary;
const F: Origin = Origin::FocusPrimary;
const C: Origin = Origin::Clipboard;

async fn run(sources: &[&dyn SelectionSource]) -> Captured {
    capture(sources, STEP_TIMEOUT).await
}

#[tokio::test]
async fn primary_wins_and_later_steps_are_skipped() {
    let primary = Fake::text(P, "Merhaba dünya");
    let focus = Fake::text(F, "focus");
    let clipboard = Fake::text(C, "clip");
    let got = run(&[&primary, &focus, &clipboard]).await;
    assert_eq!(got.text, "Merhaba dünya");
    assert_eq!(got.origin, P);
    assert!(!got.truncated);
    assert_eq!((focus.calls(), clipboard.calls()), (0, 0));
}

#[tokio::test]
async fn empty_primary_falls_back_to_focus_read() {
    let primary = Fake::new(P, Behavior::Nothing);
    let focus = Fake::text(F, "selected in GNOME");
    let clipboard = Fake::text(C, "clip");
    let got = run(&[&primary, &focus, &clipboard]).await;
    assert_eq!((got.text.as_str(), got.origin), ("selected in GNOME", F));
    assert_eq!(clipboard.calls(), 0);
}

#[tokio::test]
async fn failing_steps_fall_back_to_clipboard() {
    let primary = Fake::new(P, Behavior::Fails);
    let focus = Fake::new(F, Behavior::Fails);
    let clipboard = Fake::text(C, "Good morning");
    let got = run(&[&primary, &focus, &clipboard]).await;
    assert_eq!((got.text.as_str(), got.origin), ("Good morning", C));
    assert_eq!((primary.calls(), focus.calls()), (1, 1));
}

#[tokio::test]
async fn whitespace_only_primary_is_empty() {
    let primary = Fake::text(P, " \n\t  \r\n ");
    let clipboard = Fake::text(C, "Good morning");
    let got = run(&[&primary, &clipboard]).await;
    assert_eq!((got.text.as_str(), got.origin), ("Good morning", C));
}

#[tokio::test]
async fn nothing_anywhere_is_empty() {
    let primary = Fake::new(P, Behavior::Nothing);
    let focus = Fake::new(F, Behavior::Fails);
    let clipboard = Fake::new(C, Behavior::Nothing);
    let got = run(&[&primary, &focus, &clipboard]).await;
    assert_eq!(got, Captured::empty());
    assert!(got.is_empty());
    assert_eq!(run(&[]).await, Captured::empty());
}

#[tokio::test]
async fn image_only_clipboard_is_empty() {
    // A data-control source returns no payload when pick_text_mime finds no text type.
    assert_eq!(pick_text_mime(&["image/png".to_owned()]), None);
    let primary = Fake::new(P, Behavior::Nothing);
    let clipboard = Fake::new(C, Behavior::Nothing);
    assert_eq!(run(&[&primary, &clipboard]).await.origin, Origin::Empty);
}

#[tokio::test]
async fn unanswered_step_times_out_and_falls_through() {
    let primary = Fake::new(P, Behavior::Hangs);
    let focus = Fake::new(F, Behavior::Hangs);
    let clipboard = Fake::text(C, "Good morning");
    let start = Instant::now();
    let got = run(&[&primary, &focus, &clipboard]).await;
    let elapsed = start.elapsed();
    assert_eq!((got.text.as_str(), got.origin), ("Good morning", C));
    assert!(elapsed >= 2 * STEP_TIMEOUT, "{elapsed:?}");
    assert!(
        elapsed < 2 * STEP_TIMEOUT + Duration::from_millis(200),
        "{elapsed:?}"
    );
}

#[tokio::test]
async fn custom_step_timeout() {
    let primary = Fake::new(P, Behavior::Hangs);
    let start = Instant::now();
    let got = capture(&[&primary], Duration::from_millis(20)).await;
    assert_eq!(got.origin, Origin::Empty);
    assert!(start.elapsed() < Duration::from_millis(150));
}

#[test]
fn mime_preference() {
    let offer = |m: &[&str]| m.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let rich = offer(&["text/html", "text/plain;charset=utf-8"]);
    assert_eq!(pick_text_mime(&rich), Some("text/plain;charset=utf-8"));
    let all = offer(&[
        "text/plain",
        "UTF8_STRING",
        "TEXT",
        "text/plain;charset=utf-8",
    ]);
    assert_eq!(pick_text_mime(&all), Some("text/plain;charset=utf-8"));
    assert_eq!(
        pick_text_mime(&offer(&["text/plain", "UTF8_STRING"])),
        Some("UTF8_STRING")
    );
    assert_eq!(
        pick_text_mime(&offer(&["text/html", "text/plain"])),
        Some("text/plain")
    );
    assert_eq!(
        pick_text_mime(&offer(&["text/plain; charset=UTF-8"])),
        Some("text/plain; charset=UTF-8")
    );
    assert_eq!(
        pick_text_mime(&offer(&["text/html", "image/png", "TEXT", "STRING"])),
        None
    );
    assert_eq!(pick_text_mime(&[]), None);
}

#[test]
fn normalize_trims_but_keeps_line_breaks() {
    let n = |s: &str| normalize(&Payload::text(s));
    assert_eq!(
        n("\n  Hello world  \n"),
        Some(("Hello world".into(), false))
    );
    assert_eq!(
        n("  line one\n\n  line two\r\n"),
        Some(("line one\n\n  line two".into(), false))
    );
    assert_eq!(n(" \n\t "), None);
    assert_eq!(n(""), None);
}

#[test]
fn normalize_decodes_lossily() {
    let payload = Payload::bytes("text/plain", b"caf\xe9 ok".to_vec());
    let (text, _) = normalize(&payload).unwrap();
    assert_eq!(text, "caf\u{fffd} ok");
}

#[test]
fn normalize_truncates_by_characters() {
    let long = "x".repeat(25_000);
    let (text, truncated) = normalize(&Payload::text(long)).unwrap();
    assert_eq!(text.chars().count(), MAX_SOURCE_CHARS);
    assert!(truncated);

    let wide = "ü".repeat(MAX_SOURCE_CHARS + 1);
    let (text, truncated) = normalize(&Payload::text(wide)).unwrap();
    assert_eq!(text.chars().count(), MAX_SOURCE_CHARS);
    assert_eq!(text.len(), 2 * MAX_SOURCE_CHARS);
    assert!(truncated);

    let exact = format!("  {}  ", "é".repeat(MAX_SOURCE_CHARS));
    let (text, truncated) = normalize(&Payload::text(exact)).unwrap();
    assert_eq!(text.chars().count(), MAX_SOURCE_CHARS);
    assert!(!truncated);

    let mut capped = Payload::text("short");
    capped.capped = true;
    assert_eq!(normalize(&capped), Some(("short".into(), true)));
}

#[tokio::test]
async fn oversized_selection_sets_truncated() {
    let primary = Fake::text(P, &"y".repeat(25_000));
    let got = run(&[&primary]).await;
    assert!(got.truncated);
    assert_eq!(got.text.chars().count(), MAX_SOURCE_CHARS);
}

#[test]
fn debug_output_hides_text() {
    let secret = "very-secret-selection";
    let captured = Captured {
        text: secret.into(),
        truncated: false,
        origin: P,
    };
    assert!(!format!("{captured:?}").contains(secret));
    assert!(!format!("{:?}", Payload::text(secret)).contains(secret));
}

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn captured_text_never_reaches_logs() {
    let logs = LogBuffer::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let marker = "QT-LOG-MARKER-ŞĞÜ";
    let blank = Fake::text(P, "   ");
    let failing = Fake::new(F, Behavior::Fails);
    let clipboard = Fake::new(
        C,
        Behavior::Gives(Payload::bytes(
            "text/plain;charset=utf-8",
            format!("  {marker}\n{marker}  ").into_bytes(),
        )),
    );
    let got = run(&[&blank, &failing, &clipboard]).await;
    assert!(got.text.contains(marker));
    tracing::debug!(?got, "after capture");

    let logs =
        String::from_utf8_lossy(&logs.0.lock().unwrap_or_else(|e| e.into_inner())).into_owned();
    assert!(
        logs.contains("captured selection"),
        "logging was not captured: {logs}"
    );
    assert!(logs.contains("text/plain;charset=utf-8"));
    assert!(
        !logs.contains(marker),
        "captured text leaked into logs: {logs}"
    );
    assert!(!logs.contains("QT-LOG-MARKER"));
}
