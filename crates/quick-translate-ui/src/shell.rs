//! The interface between the daemon and the popup shells (design D3, D4).
//!
//! The daemon sends [`ShellCommand`]s and receives [`ShellEvent`]s. Both channels are unbounded,
//! so the daemon never waits on the UI and the UI never waits on the daemon.

use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::stream;
use iced::{Size, Subscription, Theme};
use quick_translate_backends::Translator;
use tokio::sync::mpsc;

use crate::popup::OpenOptions;

/// Smallest popup that keeps both panes and the header controls usable (logical px).
pub const MIN_SIZE: Size = Size::new(480.0, 200.0);

/// Distance between the pointer and the popup's top-left corner (logical px).
pub const POINTER_OFFSET: f32 = 12.0;

/// Identifies one activation. Events for an older session are stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u64);

/// Who reads the selection for a popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    /// The daemon captures through data-control and sends [`ShellCommand::Captured`].
    Daemon,
    /// The shell reads PRIMARY, then CLIPBOARD, through the toolkit once it has keyboard focus.
    Focus,
}

/// Everything a shell needs to show a popup.
#[derive(Clone)]
pub struct ShowRequest {
    pub session: SessionId,
    /// XDG activation token from the trigger, used by the window shell to get focus.
    pub activation_token: Option<String>,
    pub open: OpenOptions,
    /// Persisted popup size (logical px).
    pub size: Size,
    pub translator: Arc<dyn Translator>,
    pub capture: CaptureMode,
    /// A notice shown in the popup, e.g. a malformed config file.
    pub notice: Option<String>,
    /// When the trigger reached the daemon, for the hot-path latency logs.
    pub triggered_at: Instant,
}

impl std::fmt::Debug for ShowRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShowRequest")
            .field("session", &self.session)
            .field("activation_token", &self.activation_token.is_some())
            .field("size", &self.size)
            .field("capture", &self.capture)
            .finish_non_exhaustive()
    }
}

/// Daemon to shell.
#[derive(Debug, Clone)]
pub enum ShellCommand {
    /// Show a popup. A visible popup is replaced.
    Show(Box<ShowRequest>),
    /// Hide the popup if it is visible.
    Hide,
    /// Selection capture finished for `session` ([`CaptureMode::Daemon`]).
    Captured {
        session: SessionId,
        text: String,
        truncated: bool,
    },
    /// The system color scheme changed.
    SetTheme(Theme),
    /// Close everything and leave the event loop.
    Quit,
}

/// Shell to daemon.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellEvent {
    /// The popup surface for `session` is mapped.
    Shown {
        session: SessionId,
    },
    /// The popup for `session` is gone (Esc, copy, outside click, focus loss or Hide).
    Closed {
        session: SessionId,
    },
    PersistTarget(String),
    PersistSplitter(f32),
    PersistSize(Size),
    OpenSettings,
}

/// The daemon's end of the shell channels.
#[derive(Debug)]
pub struct DaemonEnd {
    pub commands: mpsc::UnboundedSender<ShellCommand>,
    pub events: mpsc::UnboundedReceiver<ShellEvent>,
}

/// The shell's end of the shell channels.
#[derive(Debug, Clone)]
pub struct ShellEnd {
    pub commands: Inbox,
    pub events: mpsc::UnboundedSender<ShellEvent>,
}

impl ShellEnd {
    /// Reports an event. A daemon that is gone is not an error for the UI.
    pub fn send(&self, event: ShellEvent) {
        let _ = self.events.send(event);
    }
}

/// Connected channel ends.
pub fn channel() -> (DaemonEnd, ShellEnd) {
    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    (
        DaemonEnd {
            commands: commands_tx,
            events: events_rx,
        },
        ShellEnd {
            commands: Inbox(Arc::new(Mutex::new(Some(commands_rx)))),
            events: events_tx,
        },
    )
}

/// The command receiver, handed to the shell's subscription exactly once.
#[derive(Debug, Clone)]
pub struct Inbox(Arc<Mutex<Option<mpsc::UnboundedReceiver<ShellCommand>>>>);

impl Hash for Inbox {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // One inbox per process: a constant identity keeps the subscription alive.
        "quick-translate-shell-inbox".hash(state);
    }
}

impl Inbox {
    /// The receiver itself, for a host that is not an iced program (tests, headless runs).
    pub fn take(&self) -> Option<mpsc::UnboundedReceiver<ShellCommand>> {
        self.0.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Commands from the daemon. The stream ends when the daemon drops its sender.
    pub fn subscription(&self) -> Subscription<ShellCommand> {
        Subscription::run_with(self.clone(), |inbox| {
            let receiver = inbox.0.lock().ok().and_then(|mut slot| slot.take());
            stream::unfold(receiver, |receiver| async move {
                let mut receiver = receiver?;
                let command = receiver.recv().await?;
                Some((command, Some(receiver)))
            })
        })
    }
}

/// Milliseconds since `since`, for the hot-path debug logs.
pub fn elapsed_ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}
