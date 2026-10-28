//! The single trigger path (task 6.4): portal activations, `--toggle`/`--show` and evdev combos
//! all end in [`Popups::toggle`] or [`Popups::show`]. Both only send to the UI and spawn the
//! capture and the connection warm-up; they never wait on capture, translation, network or disk.

use std::sync::Arc;
use std::time::Instant;

use futures::future::BoxFuture;
use iced::{Size, Theme};
use quick_translate_backends::Translator;
use quick_translate_core::Config;
use quick_translate_platform::selection::Captured;
use quick_translate_ui::popup::{BackendInfo, Languages, OpenOptions};
use quick_translate_ui::shell::{CaptureMode, SessionId, ShellCommand, ShellEvent, ShowRequest};
use tokio::sync::mpsc;

/// Where commands for the popup shell go.
pub trait Ui: Clone + Send + Sync + 'static {
    fn send(&self, command: ShellCommand);
}

impl Ui for mpsc::UnboundedSender<ShellCommand> {
    fn send(&self, command: ShellCommand) {
        if mpsc::UnboundedSender::send(self, command).is_err() {
            tracing::debug!("popup shell is gone");
        }
    }
}

/// Background selection capture (data-control). Without one the shell reads the selection
/// itself once it has keyboard focus.
pub trait Capture: Send + Sync + 'static {
    fn capture(&self) -> BoxFuture<'static, Captured>;
}

/// The active translation backend.
#[derive(Clone)]
pub struct Backend {
    pub translator: Arc<dyn Translator>,
    pub info: BackendInfo,
}

/// A change the popup asks to persist in `config.toml`.
#[derive(Debug, Clone, PartialEq)]
pub enum Persist {
    Target(String),
    Splitter(f32),
    Size(Size),
}

/// Popup visibility and the per-activation request.
pub struct Popups<U: Ui> {
    ui: U,
    capture: Option<Arc<dyn Capture>>,
    backend: Backend,
    config: Config,
    theme: Theme,
    notice: Option<String>,
    visible: Option<SessionId>,
    next_session: u64,
}

impl<U: Ui> Popups<U> {
    pub fn new(
        ui: U,
        capture: Option<Arc<dyn Capture>>,
        backend: Backend,
        config: Config,
        theme: Theme,
    ) -> Self {
        Self {
            ui,
            capture,
            backend,
            config,
            theme,
            notice: None,
            visible: None,
            next_session: 0,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible.is_some()
    }

    /// Shows the popup, or hides it when it is visible.
    pub fn toggle(&mut self, activation_token: Option<String>, triggered_at: Instant) {
        if self.visible.is_some() {
            tracing::debug!("toggle: hiding the popup");
            self.hide();
        } else {
            self.show(activation_token, triggered_at);
        }
    }

    /// Shows the popup for the current selection, replacing a visible one.
    pub fn show(&mut self, activation_token: Option<String>, triggered_at: Instant) {
        self.next_session += 1;
        let session = SessionId(self.next_session);
        self.visible = Some(session);
        let capture = match &self.capture {
            Some(_) => CaptureMode::Daemon,
            None => CaptureMode::Focus,
        };
        tracing::debug!(
            session = session.0,
            activation_token = activation_token.is_some(),
            ?capture,
            "hot path: trigger received"
        );
        let request = ShowRequest {
            session,
            activation_token,
            open: self.open_options(),
            size: Size::new(self.config.popup.width, self.config.popup.height),
            translator: self.backend.translator.clone(),
            capture,
            notice: self.notice.clone(),
            triggered_at,
        };
        self.ui.send(ShellCommand::Show(Box::new(request)));

        if let Some(capture) = &self.capture {
            let read = capture.capture();
            let ui = self.ui.clone();
            tokio::spawn(async move {
                let captured = read.await;
                ui.send(ShellCommand::Captured {
                    session,
                    text: captured.text,
                    truncated: captured.truncated,
                });
            });
        }
        if self.backend.info.configured {
            let translator = self.backend.translator.clone();
            tokio::spawn(async move { translator.warm_up().await });
        }
    }

    pub fn hide(&mut self) {
        if self.visible.take().is_some() {
            self.ui.send(ShellCommand::Hide);
        }
    }

    /// Handles an event from the shell. Returns what must be written to `config.toml`.
    pub fn shell_event(&mut self, event: ShellEvent) -> Option<Persist> {
        match event {
            ShellEvent::Shown { .. } | ShellEvent::OpenSettings => None,
            ShellEvent::Closed { session } => {
                if self.visible == Some(session) {
                    self.visible = None;
                }
                None
            }
            ShellEvent::PersistTarget(code) => {
                self.config.target_lang = Some(code.clone());
                Some(Persist::Target(code))
            }
            ShellEvent::PersistSplitter(ratio) => {
                self.config.popup.splitter = ratio;
                Some(Persist::Splitter(ratio))
            }
            ShellEvent::PersistSize(size) => {
                self.config.popup.width = size.width;
                self.config.popup.height = size.height;
                Some(Persist::Size(size))
            }
        }
    }

    pub fn set_theme(&mut self, theme: Theme) {
        if theme != self.theme {
            self.theme = theme.clone();
            self.ui.send(ShellCommand::SetTheme(theme));
        }
    }

    /// Applies a reloaded config (target, splitter and size for the next popup).
    pub fn set_config(&mut self, config: Config) {
        self.config = config;
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn set_backend(&mut self, backend: Backend) {
        self.backend = backend;
    }

    /// A notice shown in every popup, e.g. a malformed config file.
    pub fn set_notice(&mut self, notice: Option<String>) {
        self.notice = notice;
    }

    /// Asks the shell to leave its event loop.
    pub fn quit(&mut self) {
        self.visible = None;
        self.ui.send(ShellCommand::Quit);
    }

    fn open_options(&self) -> OpenOptions {
        OpenOptions {
            target_lang: self.config.effective_target_lang().to_owned(),
            languages: Languages::deepl(),
            backend: self.backend.info.clone(),
            splitter: self.config.popup.splitter,
            theme: self.theme.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use quick_translate_backends::{ChunkStream, Request};
    use quick_translate_platform::selection::Origin;
    use tokio_util::sync::CancellationToken;

    use super::*;

    /// Records every command, like the real shell channel would deliver them.
    #[derive(Clone, Default)]
    struct FakeUi(Arc<Mutex<Vec<ShellCommand>>>);

    impl Ui for FakeUi {
        fn send(&self, command: ShellCommand) {
            self.0.lock().expect("lock").push(command);
        }
    }

    impl FakeUi {
        fn take(&self) -> Vec<String> {
            self.0
                .lock()
                .expect("lock")
                .drain(..)
                .map(|c| match c {
                    ShellCommand::Show(r) => format!("show {}", r.session.0),
                    ShellCommand::Hide => "hide".into(),
                    ShellCommand::Captured { session, text, .. } => {
                        format!("captured {} {text}", session.0)
                    }
                    ShellCommand::SetTheme(t) => format!("theme {t}"),
                    ShellCommand::Quit => "quit".into(),
                })
                .collect()
        }

        fn last_show(&self) -> Option<ShowRequest> {
            self.0
                .lock()
                .expect("lock")
                .iter()
                .rev()
                .find_map(|c| match c {
                    ShellCommand::Show(r) => Some((**r).clone()),
                    _ => None,
                })
        }
    }

    /// A selection owner that never answers.
    struct StuckCapture;

    impl Capture for StuckCapture {
        fn capture(&self) -> BoxFuture<'static, Captured> {
            Box::pin(std::future::pending())
        }
    }

    struct FixedCapture(&'static str);

    impl Capture for FixedCapture {
        fn capture(&self) -> BoxFuture<'static, Captured> {
            let text = self.0.to_owned();
            Box::pin(async move {
                Captured {
                    text,
                    truncated: false,
                    origin: Origin::DataControlPrimary,
                }
            })
        }
    }

    /// A backend whose requests and warm-ups never finish.
    #[derive(Default)]
    struct HangingTranslator {
        warm_ups: AtomicUsize,
    }

    impl Translator for HangingTranslator {
        fn translate(
            &self,
            _req: Request,
            _cancel: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = quick_translate_backends::Result<ChunkStream>> + Send + '_>>
        {
            Box::pin(std::future::pending())
        }

        fn warm_up(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            self.warm_ups.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        }
    }

    fn popups(
        capture: Option<Arc<dyn Capture>>,
    ) -> (Popups<FakeUi>, FakeUi, Arc<HangingTranslator>) {
        let ui = FakeUi::default();
        let translator = Arc::new(HangingTranslator::default());
        let backend = Backend {
            translator: translator.clone(),
            info: BackendInfo {
                label: "DeepL".into(),
                configured: true,
                reports_detected: true,
            },
        };
        let popups = Popups::new(
            ui.clone(),
            capture,
            backend,
            Config::default(),
            Theme::Light,
        );
        (popups, ui, translator)
    }

    #[tokio::test]
    async fn toggle_shows_then_hides_without_waiting() {
        let (mut popups, ui, translator) = popups(Some(Arc::new(StuckCapture)));
        let started = Instant::now();
        popups.toggle(None, Instant::now());
        assert!(popups.is_visible());
        popups.toggle(None, Instant::now());
        assert!(!popups.is_visible());
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "toggle waited on capture or the backend"
        );
        assert_eq!(ui.take(), ["show 1", "hide"]);
        tokio::task::yield_now().await;
        assert_eq!(translator.warm_ups.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn every_trigger_source_shares_the_path() {
        // Portal activations, `--toggle` and evdev all call toggle(token); `--show` calls show.
        let (mut popups, ui, _) = popups(Some(Arc::new(StuckCapture)));
        popups.toggle(Some("portal-token".into()), Instant::now());
        assert_eq!(
            ui.last_show().and_then(|r| r.activation_token).as_deref(),
            Some("portal-token")
        );
        popups.show(None, Instant::now());
        assert!(popups.is_visible(), "show keeps the popup visible");
        popups.toggle(None, Instant::now());
        assert_eq!(ui.take(), ["show 1", "show 2", "hide"]);
    }

    #[tokio::test]
    async fn capture_result_follows_the_show() {
        let (mut popups, ui, _) = popups(Some(Arc::new(FixedCapture("Merhaba dünya"))));
        popups.toggle(None, Instant::now());
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(ui.take(), ["show 1", "captured 1 Merhaba dünya"]);
    }

    #[tokio::test]
    async fn without_data_control_the_shell_captures() {
        let (mut popups, ui, _) = popups(None);
        popups.toggle(None, Instant::now());
        let request = ui.last_show().expect("show");
        assert_eq!(request.capture, CaptureMode::Focus);
        tokio::task::yield_now().await;
        assert_eq!(ui.take(), ["show 1"]);
    }

    #[tokio::test]
    async fn popup_closing_itself_resets_visibility() {
        let (mut popups, ui, _) = popups(Some(Arc::new(StuckCapture)));
        popups.toggle(None, Instant::now());
        // A stale close from an earlier session changes nothing.
        popups.shell_event(ShellEvent::Closed {
            session: SessionId(0),
        });
        assert!(popups.is_visible());
        popups.shell_event(ShellEvent::Closed {
            session: SessionId(1),
        });
        assert!(!popups.is_visible());
        popups.toggle(None, Instant::now());
        assert_eq!(
            ui.take(),
            ["show 1", "show 2"],
            "the next trigger shows again"
        );
    }

    #[tokio::test]
    async fn hide_when_hidden_sends_nothing() {
        let (mut popups, ui, _) = popups(None);
        popups.hide();
        assert!(ui.take().is_empty());
    }

    #[tokio::test]
    async fn persisted_choices_apply_to_the_next_popup() {
        let (mut popups, ui, _) = popups(None);
        assert_eq!(
            popups.shell_event(ShellEvent::PersistTarget("DE".into())),
            Some(Persist::Target("DE".into()))
        );
        popups.shell_event(ShellEvent::PersistSize(Size::new(900.0, 400.0)));
        popups.shell_event(ShellEvent::PersistSplitter(0.3));
        popups.toggle(None, Instant::now());
        let request = ui.last_show().expect("show");
        assert_eq!(request.open.target_lang, "DE");
        assert_eq!(request.size, Size::new(900.0, 400.0));
        assert!((request.open.splitter - 0.3).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn theme_changes_reach_the_shell_once() {
        let (mut popups, ui, _) = popups(None);
        popups.set_theme(Theme::Light);
        popups.set_theme(Theme::Dark);
        popups.set_theme(Theme::Dark);
        assert_eq!(ui.take(), ["theme Dark"]);
        popups.toggle(None, Instant::now());
        assert_eq!(ui.last_show().map(|r| r.open.theme), Some(Theme::Dark));
    }
}
