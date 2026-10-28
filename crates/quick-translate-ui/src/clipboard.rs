//! Toolkit clipboard for the focus-time PRIMARY read and for CLIPBOARD ownership (design D5).
//!
//! smithay-clipboard's `load`/`load_primary` block without a timeout when the selection owner
//! never answers, and only work once the clipboard's own `wl_keyboard` has seen `enter`. So reads
//! run on reader threads that retry the focus gates until a deadline, the async caller times out
//! on its own, and a reader stuck on a silent owner is replaced instead of queued behind.
//! Copies go through a separate instance, so a stuck reader never blocks them, and stay served
//! after the popup closes because the instance lives on the connection, not on a window.
//!
//! Selection text is never logged: only origins and lengths.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use quick_translate_platform::selection::{
    Origin, Payload, STEP_TIMEOUT, SelectionSource, SourceError,
};
use tokio::sync::oneshot;
use wayland_client::Connection;

use crate::window::raw::DisplayPtr;

/// Pause between attempts while a focus gate is closed.
const RETRY_INTERVAL: Duration = Duration::from_millis(2);
/// How long an empty selection is retried: its offer can lag the focus event.
const EMPTY_GRACE: Duration = Duration::from_millis(30);
/// Readers that may hang on owners that never answer before reads fail fast.
const MAX_STUCK_READERS: usize = 3;
/// Toolkit clipboard instances replaced after their worker died, before giving up.
const MAX_REVIVALS: usize = 8;
/// Extra time the async caller waits past the reader's deadline.
const REPLY_SLACK: Duration = Duration::from_millis(10);

/// Which Wayland selection to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Primary,
    Clipboard,
}

/// The blocking calls of one toolkit clipboard instance.
trait Loader: Send {
    fn load(&mut self, which: Which) -> io::Result<String>;
}

trait Storer: Send {
    fn store(&self, text: String);
}

/// One smithay-clipboard instance. With a connection it keeps the display alive and shuts down
/// normally. On a foreign display it is never shut down: the display may already be gone when
/// the process exits, and a worker woken to tear down would touch it.
struct Smithay {
    clipboard: Option<smithay_clipboard::Clipboard>,
    connection: Option<Connection>,
}

impl Smithay {
    fn new(display: DisplayPtr, connection: Option<Connection>) -> Self {
        Self {
            clipboard: Some(display.clipboard()),
            connection,
        }
    }
}

impl Drop for Smithay {
    fn drop(&mut self) {
        let clipboard = self.clipboard.take();
        if self.connection.is_none() {
            std::mem::forget(clipboard);
        }
    }
}

impl Loader for Smithay {
    fn load(&mut self, which: Which) -> io::Result<String> {
        let clipboard = self
            .clipboard
            .as_ref()
            .ok_or_else(|| io::Error::other("clipboard is gone"))?;
        match which {
            Which::Primary => clipboard.load_primary(),
            Which::Clipboard => clipboard.load(),
        }
    }
}

impl Storer for Smithay {
    fn store(&self, text: String) {
        if let Some(clipboard) = &self.clipboard {
            clipboard.store(text);
        }
    }
}

type Reply = io::Result<Option<String>>;

struct Request {
    which: Which,
    deadline: Instant,
    reply: oneshot::Sender<Reply>,
}

struct Reader {
    requests: mpsc::Sender<Request>,
    busy: Arc<AtomicBool>,
}

type MakeLoader = Box<dyn Fn() -> Box<dyn Loader> + Send + Sync>;
type MakeStorer = Box<dyn Fn() -> Box<dyn Storer> + Send + Sync>;

/// What the reader threads share: the instance factories and the writer.
struct Shared {
    make_loader: MakeLoader,
    make_storer: MakeStorer,
    writer: Mutex<Box<dyn Storer>>,
    revivals: AtomicUsize,
}

impl Shared {
    /// A worker died (smithay-clipboard panics when a pointer goes away mid-frame). Instances
    /// on the connection die together, so the writer is replaced too.
    fn revive(&self) -> bool {
        if self.revivals.fetch_add(1, Ordering::AcqRel) >= MAX_REVIVALS {
            return false;
        }
        tracing::warn!("toolkit clipboard worker died, starting a new one");
        if let Ok(mut writer) = self.writer.lock() {
            *writer = (self.make_storer)();
        }
        true
    }
}

fn is_dead(reply: &Reply) -> bool {
    matches!(reply, Err(error) if error.to_string().contains("clipboard is dead"))
}

/// Reader threads, each owning one loader. Idle readers block on their request channel.
struct Readers {
    shared: Arc<Shared>,
    readers: Mutex<Vec<Reader>>,
}

impl Readers {
    fn new(make_loader: MakeLoader, make_storer: MakeStorer) -> Self {
        let writer = Mutex::new(make_storer());
        let readers = Self {
            shared: Arc::new(Shared {
                make_loader,
                make_storer,
                writer,
                revivals: AtomicUsize::new(0),
            }),
            readers: Mutex::new(Vec::new()),
        };
        // The first instance must exist before focus arrives so its keyboard sees `enter`.
        if let Ok(mut list) = readers.readers.lock()
            && let Some(reader) = readers.spawn()
        {
            list.push(reader);
        }
        readers
    }

    fn spawn(&self) -> Option<Reader> {
        let mut loader = (self.shared.make_loader)();
        let (requests, inbox) = mpsc::channel::<Request>();
        let busy = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&busy);
        let shared = Arc::clone(&self.shared);
        thread::Builder::new()
            .name("qt-selection-reader".into())
            .spawn(move || {
                while let Ok(request) = inbox.recv() {
                    let mut reply = serve(&mut *loader, request.which, request.deadline);
                    if is_dead(&reply) && shared.revive() {
                        loader = (shared.make_loader)();
                        reply = serve(&mut *loader, request.which, request.deadline);
                    }
                    let _ = request.reply.send(reply);
                    flag.store(false, Ordering::Release);
                }
            })
            .map_err(|e| tracing::warn!(error = %e, "cannot start a selection reader"))
            .ok()?;
        Some(Reader { requests, busy })
    }

    /// Hands the request to an idle reader, starting one if all are busy (stuck).
    fn submit(&self, mut request: Request) {
        let Ok(mut list) = self.readers.lock() else {
            let _ = request
                .reply
                .send(Err(io::Error::other("selection readers poisoned")));
            return;
        };
        for reader in list.iter() {
            if reader
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue;
            }
            match reader.requests.send(request) {
                Ok(()) => return,
                Err(mpsc::SendError(back)) => request = back,
            }
        }
        // A reader whose thread is gone no longer holds its flag.
        list.retain(|reader| Arc::strong_count(&reader.busy) > 1);
        if list.len() > MAX_STUCK_READERS {
            tracing::debug!(readers = list.len(), "all selection readers are stuck");
            let _ = request
                .reply
                .send(Err(io::Error::other("selection readers are stuck")));
            return;
        }
        let Some(reader) = self.spawn() else {
            let _ = request
                .reply
                .send(Err(io::Error::other("no selection reader")));
            return;
        };
        if !list.is_empty() {
            tracing::debug!(
                readers = list.len() + 1,
                "started a reader next to a stuck one"
            );
        }
        reader.busy.store(true, Ordering::Release);
        if let Err(mpsc::SendError(request)) = reader.requests.send(request) {
            let _ = request
                .reply
                .send(Err(io::Error::other("selection reader died")));
        }
        list.push(reader);
    }
}

/// Why one attempt failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// Focus, seat or offer not there yet: retry until the deadline.
    Gate,
    /// No selection (yet): retry for [`EMPTY_GRACE`], then report empty.
    Empty,
    /// Nothing textual offered: report empty now.
    NoText,
    Fatal,
}

fn classify(error: &io::Error) -> Failure {
    if error.kind() == io::ErrorKind::NotFound {
        return Failure::NoText;
    }
    let message = error.to_string();
    if message.contains("doesn't have focus")
        || message.contains("no events received on any seat")
        || message.contains("offer is not ready")
    {
        Failure::Gate
    } else if message.contains("selection is empty") {
        Failure::Empty
    } else {
        Failure::Fatal
    }
}

/// One read with retries, on a reader thread. Makes at least one attempt.
fn serve(loader: &mut dyn Loader, which: Which, deadline: Instant) -> Reply {
    let mut empty_since: Option<Instant> = None;
    loop {
        let error = match loader.load(which) {
            Ok(text) => return Ok(Some(text)),
            Err(error) => error,
        };
        let now = Instant::now();
        let limit = match classify(&error) {
            Failure::NoText => return Ok(None),
            Failure::Fatal => return Err(error),
            Failure::Gate => {
                empty_since = None;
                deadline
            }
            Failure::Empty => deadline.min(*empty_since.get_or_insert(now) + EMPTY_GRACE),
        };
        if now + RETRY_INTERVAL > limit {
            return match classify(&error) {
                Failure::Empty => Ok(None),
                _ => Err(error),
            };
        }
        thread::sleep(RETRY_INTERVAL);
    }
}

/// Toolkit PRIMARY/CLIPBOARD access on one Wayland connection.
#[derive(Clone)]
pub struct FocusClipboard {
    inner: Arc<Readers>,
}

impl std::fmt::Debug for FocusClipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FocusClipboard").finish_non_exhaustive()
    }
}

impl FocusClipboard {
    /// Clipboard on `connection`, which it keeps alive.
    pub fn from_connection(connection: &Connection) -> Option<Self> {
        let display = DisplayPtr::from_connection(connection)?;
        let (reader, writer) = (connection.clone(), connection.clone());
        Some(Self::with(
            Box::new(move || Box::new(Smithay::new(display, Some(reader.clone())))),
            Box::new(move || Box::new(Smithay::new(display, Some(writer.clone())))),
        ))
    }

    /// Clipboard on a display that lives as long as the process (winit's, in the window shell).
    pub(crate) fn from_display(display: DisplayPtr) -> Self {
        Self::with(
            Box::new(move || Box::new(Smithay::new(display, None))),
            Box::new(move || Box::new(Smithay::new(display, None))),
        )
    }

    fn with(make_loader: MakeLoader, make_storer: MakeStorer) -> Self {
        Self {
            inner: Arc::new(Readers::new(make_loader, make_storer)),
        }
    }

    /// Reads a selection within `budget`. `Ok(None)` means empty or no text offered.
    pub fn read(
        &self,
        which: Which,
        budget: Duration,
    ) -> impl Future<Output = io::Result<Option<String>>> + Send + 'static {
        let deadline = Instant::now() + budget;
        let (reply, answer) = oneshot::channel();
        self.inner.submit(Request {
            which,
            deadline,
            reply,
        });
        async move {
            match tokio::time::timeout_at((deadline + REPLY_SLACK).into(), answer).await {
                Ok(Ok(reply)) => reply,
                Ok(Err(_)) => Err(io::Error::other("selection reader died")),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "selection owner did not answer",
                )),
            }
        }
    }

    /// Puts `text` on CLIPBOARD and keeps serving it. Never blocks. Only takes effect while
    /// one of this connection's surfaces has keyboard focus.
    pub fn store(&self, text: String) {
        tracing::debug!(chars = text.chars().count(), "storing CLIPBOARD");
        if let Ok(writer) = self.inner.shared.writer.lock() {
            writer.store(text);
        }
    }

    /// The focus-time PRIMARY step of the capture pipeline.
    pub fn primary_source(&self) -> FocusSource {
        FocusSource::new(self.clone(), Which::Primary)
    }

    /// The CLIPBOARD step of the capture pipeline.
    pub fn clipboard_source(&self) -> FocusSource {
        FocusSource::new(self.clone(), Which::Clipboard)
    }
}

/// A [`SelectionSource`] reading through a [`FocusClipboard`].
#[derive(Debug, Clone)]
pub struct FocusSource {
    clipboard: FocusClipboard,
    which: Which,
    budget: Duration,
}

impl FocusSource {
    fn new(clipboard: FocusClipboard, which: Which) -> Self {
        Self {
            clipboard,
            which,
            budget: STEP_TIMEOUT,
        }
    }

    /// How long closed focus gates are retried (defaults to [`STEP_TIMEOUT`]). Zero makes a
    /// single attempt, for when focus was not granted.
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }
}

impl SelectionSource for FocusSource {
    fn origin(&self) -> Origin {
        match self.which {
            Which::Primary => Origin::FocusPrimary,
            Which::Clipboard => Origin::Clipboard,
        }
    }

    fn read(&self) -> BoxFuture<'_, Result<Option<Payload>, SourceError>> {
        let read = self.clipboard.read(self.which, self.budget);
        Box::pin(async move {
            read.await
                .map(|text| text.map(Payload::text))
                .map_err(Into::into)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    const FOCUS: &str = "client doesn't have focus";
    const SEAT: &str = "no events received on any seat";
    const EMPTY: &str = "selection is empty";

    enum Step {
        Fail(&'static str),
        NoMime,
        Text(&'static str),
        /// Blocks until the gate is opened, like an owner that never answers.
        Hang(Arc<Mutex<mpsc::Receiver<()>>>),
    }

    struct Script {
        steps: VecDeque<Step>,
        /// CLIPBOARD reads answer this instead of following the steps.
        clipboard: Option<&'static str>,
        /// Used once the script is exhausted.
        then: &'static str,
        loads: Arc<AtomicUsize>,
    }

    impl Loader for Script {
        fn load(&mut self, which: Which) -> io::Result<String> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            if let (Which::Clipboard, Some(text)) = (which, self.clipboard) {
                return Ok(text.to_owned());
            }
            match self.steps.pop_front() {
                Some(Step::Fail(message)) => Err(io::Error::other(message)),
                Some(Step::NoMime) => Err(io::Error::new(io::ErrorKind::NotFound, "no mime")),
                Some(Step::Text(text)) => Ok(text.to_owned()),
                Some(Step::Hang(gate)) => {
                    let _ = gate.lock().expect("gate").recv();
                    Ok("late".into())
                }
                None => Err(io::Error::other(self.then)),
            }
        }
    }

    fn script(steps: Vec<Step>, then: &'static str) -> (Script, Arc<AtomicUsize>) {
        let loads = Arc::new(AtomicUsize::new(0));
        let script = Script {
            steps: steps.into(),
            clipboard: None,
            then,
            loads: Arc::clone(&loads),
        };
        (script, loads)
    }

    fn in_ms(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn gate_errors_are_retried_until_text_arrives() {
        let (mut loader, loads) = script(
            vec![Step::Fail(SEAT), Step::Fail(FOCUS), Step::Text("Hallo")],
            FOCUS,
        );
        let reply = serve(&mut loader, Which::Primary, in_ms(150));
        assert_eq!(reply.expect("read").as_deref(), Some("Hallo"));
        assert_eq!(loads.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn closed_gate_fails_at_the_deadline() {
        let (mut loader, _) = script(vec![], FOCUS);
        let started = Instant::now();
        let error = serve(&mut loader, Which::Primary, in_ms(40)).expect_err("gate stays closed");
        assert!(error.to_string().contains("focus"));
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(35) && took < Duration::from_millis(80),
            "{took:?}"
        );
    }

    #[test]
    fn zero_budget_makes_one_attempt() {
        let (mut loader, loads) = script(vec![], FOCUS);
        assert!(serve(&mut loader, Which::Primary, Instant::now()).is_err());
        assert!(loads.load(Ordering::SeqCst) <= 1);
    }

    #[test]
    fn empty_selection_is_retried_briefly_then_empty() {
        let (mut loader, _) = script(vec![], EMPTY);
        let started = Instant::now();
        let reply = serve(&mut loader, Which::Primary, in_ms(150)).expect("empty is not an error");
        assert_eq!(reply, None);
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(25) && took < Duration::from_millis(100),
            "{took:?}"
        );
    }

    #[test]
    fn late_offer_within_grace_is_read() {
        let (mut loader, _) = script(
            vec![Step::Fail(EMPTY), Step::Fail(EMPTY), Step::Text("spät")],
            EMPTY,
        );
        let reply = serve(&mut loader, Which::Clipboard, in_ms(150)).expect("read");
        assert_eq!(reply.as_deref(), Some("spät"));
    }

    #[test]
    fn no_text_type_is_empty_at_once() {
        let (mut loader, loads) = script(vec![Step::NoMime], FOCUS);
        assert_eq!(
            serve(&mut loader, Which::Clipboard, in_ms(150)).expect("ok"),
            None
        );
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn other_errors_are_not_retried() {
        let (mut loader, loads) = script(vec![Step::Fail("clipboard is dead.")], FOCUS);
        assert!(serve(&mut loader, Which::Primary, in_ms(150)).is_err());
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    struct NoStore;

    impl Storer for NoStore {
        fn store(&self, _: String) {}
    }

    /// Every loader the pool creates takes its steps from the next script.
    fn pool(scripts: Vec<Vec<Step>>) -> (FocusClipboard, Arc<AtomicUsize>) {
        pool_with(scripts, None, FOCUS)
    }

    fn pool_with(
        scripts: Vec<Vec<Step>>,
        clipboard_text: Option<&'static str>,
        then: &'static str,
    ) -> (FocusClipboard, Arc<AtomicUsize>) {
        let created = Arc::new(AtomicUsize::new(0));
        let queue = Mutex::new(VecDeque::from(scripts));
        let counter = Arc::clone(&created);
        let make: MakeLoader = Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let steps = queue.lock().expect("queue").pop_front().unwrap_or_default();
            let mut loader = script(steps, then).0;
            loader.clipboard = clipboard_text;
            Box::new(loader)
        });
        (
            FocusClipboard::with(make, Box::new(|| Box::new(NoStore))),
            created,
        )
    }

    fn hang() -> (Step, mpsc::Sender<()>) {
        let (open, gate) = mpsc::channel();
        (Step::Hang(Arc::new(Mutex::new(gate))), open)
    }

    #[tokio::test]
    async fn stuck_reader_is_replaced_not_queued_behind() {
        let (stuck, release) = hang();
        let (clipboard, created) = pool(vec![
            vec![stuck, Step::Text("again")],
            vec![Step::Text("fresh")],
        ]);
        assert_eq!(
            created.load(Ordering::SeqCst),
            1,
            "first reader exists up front"
        );

        let started = Instant::now();
        let error = clipboard
            .read(Which::Primary, Duration::from_millis(30))
            .await
            .expect_err("owner never answers");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(100));

        let fresh = clipboard
            .read(Which::Primary, Duration::from_millis(30))
            .await;
        assert_eq!(fresh.expect("fresh reader").as_deref(), Some("fresh"));
        assert_eq!(created.load(Ordering::SeqCst), 2);

        // The unstuck reader becomes idle again and is reused before any new one is made.
        release.send(()).expect("release");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let reused = clipboard
            .read(Which::Primary, Duration::from_millis(30))
            .await;
        assert_eq!(reused.expect("reused reader").as_deref(), Some("again"));
        assert_eq!(created.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn too_many_stuck_readers_fail_fast() {
        let mut releases = Vec::new();
        let mut scripts = Vec::new();
        for _ in 0..=MAX_STUCK_READERS {
            let (stuck, release) = hang();
            scripts.push(vec![stuck]);
            releases.push(release);
        }
        let (clipboard, created) = pool(scripts);
        for _ in 0..=MAX_STUCK_READERS {
            let reply = clipboard
                .read(Which::Primary, Duration::from_millis(5))
                .await;
            assert!(reply.is_err());
        }
        assert_eq!(created.load(Ordering::SeqCst), MAX_STUCK_READERS + 1);

        let started = Instant::now();
        let error = clipboard
            .read(Which::Primary, Duration::from_millis(100))
            .await
            .expect_err("no reader left");
        assert!(error.to_string().contains("stuck"));
        assert!(started.elapsed() < Duration::from_millis(20));
        assert_eq!(created.load(Ordering::SeqCst), MAX_STUCK_READERS + 1);
        drop(releases);
    }

    #[tokio::test]
    async fn dead_worker_is_replaced_with_the_writer() {
        let writers = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&writers);
        let loaders = Mutex::new(VecDeque::from([
            vec![Step::Fail("clipboard is dead.")],
            vec![Step::Text("revived")],
        ]));
        let clipboard = FocusClipboard::with(
            Box::new(move || {
                let steps = loaders
                    .lock()
                    .expect("loaders")
                    .pop_front()
                    .unwrap_or_default();
                Box::new(script(steps, FOCUS).0)
            }),
            Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Box::new(NoStore)
            }),
        );
        assert_eq!(writers.load(Ordering::SeqCst), 1);
        let reply = clipboard
            .read(Which::Primary, Duration::from_millis(50))
            .await;
        assert_eq!(reply.expect("revived").as_deref(), Some("revived"));
        assert_eq!(writers.load(Ordering::SeqCst), 2, "writer replaced too");
    }

    #[tokio::test]
    async fn sources_report_origins_and_payloads() {
        let (clipboard, _) = pool(vec![vec![Step::Text(" Hallo "), Step::NoMime]]);
        let primary = clipboard.primary_source();
        let fallback = clipboard.clipboard_source();
        assert_eq!(primary.origin(), Origin::FocusPrimary);
        assert_eq!(fallback.origin(), Origin::Clipboard);
        let payload = primary.read().await.expect("read").expect("text");
        assert_eq!(payload, Payload::text(" Hallo "));
        assert_eq!(fallback.read().await.expect("read"), None);
    }

    #[tokio::test]
    async fn capture_falls_through_to_clipboard() {
        use quick_translate_platform::selection::capture;
        let (clipboard, _) = pool_with(vec![vec![]], Some(" Good morning\n"), EMPTY);
        let primary = clipboard.primary_source();
        let fallback = clipboard.clipboard_source();
        let started = Instant::now();
        let captured = capture(&[&primary, &fallback], STEP_TIMEOUT).await;
        assert_eq!(captured.origin, Origin::Clipboard);
        assert_eq!(captured.text, "Good morning");
        assert!(
            started.elapsed() < STEP_TIMEOUT,
            "empty PRIMARY only costs the grace"
        );
    }

    #[tokio::test]
    async fn no_focus_capture_is_fast_and_empty() {
        use quick_translate_platform::selection::capture;
        let (clipboard, _) = pool(vec![vec![]]);
        let primary = clipboard.primary_source().with_budget(Duration::ZERO);
        let fallback = clipboard.clipboard_source().with_budget(Duration::ZERO);
        let started = Instant::now();
        let captured = capture(&[&primary, &fallback], STEP_TIMEOUT).await;
        assert_eq!(captured.origin, Origin::Empty);
        assert!(started.elapsed() < Duration::from_millis(30));
    }
}
