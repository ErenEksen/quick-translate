//! Runs the window shell with an echo translator and drives it with Show/Hide commands, for
//! nested-compositor tests. Exits on its own (hard timeout).
//!
//! Environment (all optional):
//! - `QT_WS_CYCLES` popups to show (default 3), `QT_WS_HOLD_MS` how long each stays before
//!   Hide (default 1500, `0` never hides), `QT_WS_GAP_MS` pause between popups (default 400)
//! - `QT_WS_CAPTURE` `focus` (default) or `daemon` (sends a canned capture)
//! - `QT_WS_TOKEN` activation token passed with each Show
//! - `QT_WS_LINGER_MS` time after the last popup before Quit (default 1000)
//! - `QT_WS_TIMEOUT_S` hard timeout (default 20)
//! - `QT_WS_LOG=1` prints debug logs of the shell (never selection text)
//!
//! Every shell event is printed with the time since its Show command.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::stream;
use iced::{Size, Theme};
use quick_translate_backends::{Chunk, ChunkStream, Request, Translator};
use quick_translate_ui::popup::{BackendInfo, Languages, OpenOptions};
use quick_translate_ui::shell::{self, CaptureMode, SessionId, ShellCommand, ShowRequest};
use quick_translate_ui::window::{self, WindowShell};
use tokio_util::sync::CancellationToken;

struct Echo;

impl Translator for Echo {
    fn translate(
        &self,
        req: Request,
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = quick_translate_backends::Result<ChunkStream>> + Send + '_>>
    {
        let chunks = vec![Ok(Chunk::Delta(format!(
            "[{}] {}",
            req.target_lang, req.text
        )))];
        Box::pin(async move { Ok(Box::pin(stream::iter(chunks)) as ChunkStream) })
    }
}

fn env<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() -> iced::Result {
    let started = Instant::now();
    let cycles: u64 = env("QT_WS_CYCLES", 3);
    let hold = Duration::from_millis(env("QT_WS_HOLD_MS", 1500));
    let gap = Duration::from_millis(env("QT_WS_GAP_MS", 400));
    let linger = Duration::from_millis(env("QT_WS_LINGER_MS", 1000));
    let timeout = Duration::from_secs(env("QT_WS_TIMEOUT_S", 20));
    let capture = match std::env::var("QT_WS_CAPTURE").as_deref() {
        Ok("daemon") => CaptureMode::Daemon,
        _ => CaptureMode::Focus,
    };
    let token = std::env::var("QT_WS_TOKEN").ok();
    if std::env::var("QT_WS_LOG").is_ok_and(|v| v == "1") {
        let _ = tracing::subscriber::set_global_default(Printer(started));
    }

    std::thread::spawn(move || {
        std::thread::sleep(timeout);
        eprintln!("[window_shell] hard timeout");
        std::process::exit(3);
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    quick_translate_ui::executor::install(runtime.handle().clone());

    let (mut daemon, end) = shell::channel();
    let shown_at: Arc<Mutex<Instant>> = Arc::new(Mutex::new(Instant::now()));
    let since = Arc::clone(&shown_at);
    runtime.spawn(async move {
        while let Some(event) = daemon.events.recv().await {
            let ms = since.lock().map(|at| at.elapsed()).unwrap_or_default();
            println!(
                "[t+{:7.1}ms] event {event:?} ({:.1} ms after Show)",
                ms_since(started),
                ms.as_secs_f64() * 1000.0
            );
        }
    });
    let commands = daemon.commands;
    runtime.spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        for n in 1..=cycles {
            if let Ok(mut at) = shown_at.lock() {
                *at = Instant::now();
            }
            println!("[t+{:7.1}ms] Show #{n}", ms_since(started));
            let _ = commands.send(ShellCommand::Show(Box::new(request(
                n,
                capture,
                token.clone(),
            ))));
            if capture == CaptureMode::Daemon {
                let _ = commands.send(ShellCommand::Captured {
                    session: SessionId(n),
                    text: format!("canned capture {n}"),
                    truncated: false,
                });
            }
            if hold.is_zero() {
                tokio::time::sleep(gap).await;
                continue;
            }
            tokio::time::sleep(hold).await;
            println!("[t+{:7.1}ms] Hide #{n}", ms_since(started));
            let _ = commands.send(ShellCommand::Hide);
            tokio::time::sleep(gap).await;
        }
        tokio::time::sleep(linger).await;
        println!("[t+{:7.1}ms] Quit", ms_since(started));
        let _ = commands.send(ShellCommand::Quit);
    });

    let result = window::run(WindowShell {
        shell: end,
        theme: Theme::Light,
    });
    println!(
        "[t+{:7.1}ms] window shell returned {result:?}",
        ms_since(started)
    );
    result
}

fn ms_since(at: Instant) -> f64 {
    at.elapsed().as_secs_f64() * 1000.0
}

fn request(n: u64, capture: CaptureMode, token: Option<String>) -> ShowRequest {
    ShowRequest {
        session: SessionId(n),
        activation_token: token,
        open: OpenOptions {
            target_lang: "DE".into(),
            languages: Languages::deepl(),
            backend: BackendInfo {
                label: "Echo".into(),
                configured: true,
                reports_detected: false,
            },
            splitter: 0.5,
            theme: Theme::Light,
        },
        size: Size::new(720.0, 320.0),
        translator: Arc::new(Echo),
        capture,
        notice: None,
        triggered_at: Instant::now(),
    }
}

/// Prints debug events of this workspace's crates with the time since start.
struct Printer(Instant);

impl tracing::Subscriber for Printer {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::TRACE
            && metadata.target().starts_with("quick_translate")
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0.insert_str(0, &format!("{value:?}"));
                } else {
                    self.0.push_str(&format!(" {}={value:?}", field.name()));
                }
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        println!("[t+{:7.1}ms] log {}", ms_since(self.0), fields.0);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}
