//! Tasks 5.2–5.4: offer tracking, real PRIMARY/CLIPBOARD capture, selection preservation and
//! log hygiene, against headless sway with `wl-copy`/`wl-paste`.
//!
//! ```sh
//! eval "$(scripts/headless-sway.sh start)"
//! cargo test -p quick-translate-platform --test selection_headless
//! ```

mod wayland_support;

use std::io;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use quick_translate_core::MAX_SOURCE_CHARS;
use quick_translate_core::capabilities::DataControl;
use quick_translate_platform::selection::{
    self, Captured, DataControlSource, Origin, STEP_TIMEOUT, SelectionSource,
};
use quick_translate_platform::wayland::{
    self, DataControlHandle, OfferInfo, Selection, WaylandDisplay,
};
use wayland_support::{headless, serial, wait_for, wl_copy, wl_paste};

const PLAIN_UTF8: &str = "text/plain;charset=utf-8";

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

/// A process-wide TRACE subscriber, so logs from the data-control thread are captured too.
fn logs() -> &'static LogBuffer {
    static LOGS: OnceLock<LogBuffer> = OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = LogBuffer::default();
        let writer = logs.clone();
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .init();
        logs
    })
}

fn log_text() -> String {
    String::from_utf8_lossy(&logs().0.lock().unwrap_or_else(|e| e.into_inner())).into_owned()
}

fn protocols(display: &WaylandDisplay) -> Vec<DataControl> {
    let caps = wayland::probe_display(display).expect("probe");
    let mut out = Vec::new();
    if let Some(v) = caps.ext_data_control {
        out.push(DataControl::Ext(v));
    }
    if let Some(v) = caps.wlr_data_control.filter(|&v| v >= 2) {
        out.push(DataControl::Wlr(v));
    }
    assert!(!out.is_empty(), "compositor offers no usable data-control");
    out
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn offer(handle: &DataControlHandle, which: Selection) -> Option<OfferInfo> {
    let s = handle.snapshot();
    match which {
        Selection::Primary => s.primary,
        Selection::Clipboard => s.clipboard,
    }
}

fn serial_of(handle: &DataControlHandle, which: Selection) -> u64 {
    offer(handle, which).map_or(0, |o| o.serial)
}

/// Set a selection with wl-copy and wait until the thread tracks the new offer.
fn set(
    display: &WaylandDisplay,
    handle: &DataControlHandle,
    which: Selection,
    args: &[&str],
    stdin: Option<&[u8]>,
) {
    let before = serial_of(handle, which);
    let mut all = Vec::new();
    if which == Selection::Primary {
        all.push("--primary");
    }
    all.extend_from_slice(args);
    wl_copy(display, &all, stdin);
    let clear = args.contains(&"--clear");
    wait_for(&format!("{which:?} update after wl-copy {all:?}"), || {
        let now = offer(handle, which);
        if clear {
            now.is_none()
        } else {
            now.is_some_and(|o| o.serial > before && !o.mime_types.is_empty())
        }
    });
}

fn set_text(display: &WaylandDisplay, handle: &DataControlHandle, which: Selection, text: &str) {
    set(display, handle, which, &[text], None);
}

fn clear(display: &WaylandDisplay, handle: &DataControlHandle, which: Selection) {
    set(display, handle, which, &["--clear"], None);
}

fn capture_with(handle: &DataControlHandle) -> Captured {
    let primary = DataControlSource::primary(handle.clone());
    let clipboard = DataControlSource::clipboard(handle.clone());
    let sources: [&dyn SelectionSource; 2] = [&primary, &clipboard];
    runtime().block_on(selection::capture(&sources, STEP_TIMEOUT))
}

/// Run `f` once per available data-control protocol.
fn each_protocol(test: &str, f: impl Fn(&WaylandDisplay, &DataControlHandle)) {
    let Some(display) = headless(test) else {
        return;
    };
    let _serial = serial();
    logs();
    for protocol in protocols(&display) {
        eprintln!("{test}: {protocol:?}");
        let handle = DataControlHandle::spawn_on(&display, protocol).expect("spawn");
        assert_eq!(handle.protocol(), protocol);
        f(&display, &handle);
        assert!(!handle.snapshot().finished);
    }
}

#[test]
fn tracks_latest_primary_offer() {
    each_protocol("tracks_latest_primary_offer", |display, handle| {
        set_text(display, handle, Selection::Primary, "foo");
        let first = offer(handle, Selection::Primary).expect("primary offer");
        assert!(
            first.mime_types.iter().any(|m| m == PLAIN_UTF8),
            "{first:?}"
        );

        set_text(display, handle, Selection::Primary, "bar");
        let second = offer(handle, Selection::Primary).expect("primary offer");
        assert!(second.serial > first.serial);

        set_text(display, handle, Selection::Clipboard, "clip");
        assert!(offer(handle, Selection::Clipboard).is_some());
        assert_eq!(offer(handle, Selection::Primary), Some(second));

        clear(display, handle, Selection::Primary);
        assert_eq!(offer(handle, Selection::Primary), None);
    });
}

#[test]
fn reads_real_primary() {
    each_protocol("reads_real_primary", |display, handle| {
        set_text(display, handle, Selection::Clipboard, "clipboard text");
        set_text(display, handle, Selection::Primary, "\n  Merhaba dünya  \n");
        let got = capture_with(handle);
        assert_eq!(got.text, "Merhaba dünya");
        assert_eq!(got.origin, Origin::DataControlPrimary);
        assert!(!got.truncated);

        let raw = runtime()
            .block_on(handle.read(Selection::Primary, STEP_TIMEOUT))
            .expect("read")
            .expect("payload");
        assert_eq!(raw.mime.as_deref(), Some(PLAIN_UTF8));
        assert_eq!(raw.bytes, "\n  Merhaba dünya  \n".as_bytes());
    });
}

#[test]
fn falls_back_to_clipboard_then_empty() {
    each_protocol("falls_back_to_clipboard_then_empty", |display, handle| {
        clear(display, handle, Selection::Primary);
        set_text(display, handle, Selection::Clipboard, "Good morning");
        let got = capture_with(handle);
        assert_eq!(
            (got.text.as_str(), got.origin),
            ("Good morning", Origin::Clipboard)
        );

        set_text(display, handle, Selection::Primary, " \n\t \n");
        let got = capture_with(handle);
        assert_eq!(
            got.origin,
            Origin::Clipboard,
            "whitespace-only PRIMARY must fall through"
        );

        clear(display, handle, Selection::Primary);
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        set(
            display,
            handle,
            Selection::Clipboard,
            &["--type", "image/png"],
            Some(png),
        );
        assert_eq!(
            offer(handle, Selection::Clipboard).map(|o| o.mime_types),
            Some(vec!["image/png".to_owned()])
        );
        assert_eq!(capture_with(handle), Captured::empty());

        clear(display, handle, Selection::Clipboard);
        assert_eq!(capture_with(handle), Captured::empty());
    });
}

#[test]
fn prefers_plain_utf8_over_rich_text() {
    each_protocol("prefers_plain_utf8_over_rich_text", |display, handle| {
        set(
            display,
            handle,
            Selection::Primary,
            &["--type", "text/html", "<b>bold</b>"],
            None,
        );
        let mime_types = offer(handle, Selection::Primary).expect("offer").mime_types;
        assert_eq!(mime_types.first().map(String::as_str), Some("text/html"));
        let raw = runtime()
            .block_on(handle.read(Selection::Primary, STEP_TIMEOUT))
            .expect("read")
            .expect("payload");
        assert_eq!(
            raw.mime.as_deref(),
            Some(PLAIN_UTF8),
            "offered {mime_types:?}"
        );
    });
}

#[test]
fn truncates_oversized_primary() {
    each_protocol("truncates_oversized_primary", |display, handle| {
        let big = "ş".repeat(25_000);
        set(
            display,
            handle,
            Selection::Primary,
            &[],
            Some(big.as_bytes()),
        );
        let got = capture_with(handle);
        assert_eq!(got.origin, Origin::DataControlPrimary);
        assert!(got.truncated);
        assert_eq!(got.text.chars().count(), MAX_SOURCE_CHARS);
    });
}

#[test]
fn capture_preserves_primary_and_clipboard() {
    each_protocol(
        "capture_preserves_primary_and_clipboard",
        |display, handle| {
            set_text(display, handle, Selection::Clipboard, "A");
            set_text(display, handle, Selection::Primary, "B");
            let before = handle.snapshot();

            assert_eq!(capture_with(handle).text, "B");
            clear(display, handle, Selection::Primary);
            let got = capture_with(handle);
            assert_eq!((got.text.as_str(), got.origin), ("A", Origin::Clipboard));
            set_text(display, handle, Selection::Primary, "B");

            std::thread::sleep(Duration::from_millis(50));
            let paste = |args: &[&str]| {
                let out = wl_paste(display, args);
                assert!(out.status.success(), "wl-paste {args:?}: {out:?}");
                String::from_utf8(out.stdout).expect("utf-8")
            };
            assert_eq!(paste(&["--no-newline"]), "A");
            assert_eq!(paste(&["--primary", "--no-newline"]), "B");
            // Capturing created no new offers: nobody (including us) set a selection.
            assert_eq!(handle.snapshot().clipboard, before.clipboard);
        },
    );
}

#[test]
fn captured_text_never_reaches_logs() {
    each_protocol("captured_text_never_reaches_logs", |display, handle| {
        let marker = "QT-HEADLESS-LOG-MARKER-çğı";
        set_text(display, handle, Selection::Primary, marker);
        tracing::debug!("capture with debug logging");
        let got = capture_with(handle);
        assert_eq!(got.text, marker);
        tracing::debug!(?got, "captured");

        clear(display, handle, Selection::Primary);
        set_text(display, handle, Selection::Clipboard, marker);
        assert_eq!(capture_with(handle).origin, Origin::Clipboard);

        let logs = log_text();
        assert!(
            logs.contains("selection read"),
            "thread logs not captured: {logs}"
        );
        assert!(logs.contains("captured selection"));
        assert!(
            !logs.contains("QT-HEADLESS-LOG-MARKER"),
            "text leaked: {logs}"
        );
    });
}
