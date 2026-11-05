//! The single trigger path (task 6.4): portal activations, `--toggle`/`--show` and evdev combos
//! all end in [`Popups::toggle`] or [`Popups::show`]. Both only send to the UI and spawn the
//! capture and the connection warm-up; they never wait on capture, translation, network or disk.

use std::sync::Arc;
use std::time::Instant;

use futures::future::BoxFuture;
use iced::{Size, Theme};
use quick_translate_backends::Translator;
use quick_translate_core::{BackendKind, Config};
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

/// The active backend and, when DeepL and an AI endpoint are both set up, the other one for
/// the popup's DeepL/AI switch.
#[derive(Clone)]
pub struct Backends {
    pub active: Backend,
    pub alternate: Option<Backend>,
}

impl Backends {
    /// Only one backend: the popup shows no switch.
    #[cfg(test)]
    pub fn single(active: Backend) -> Self {
        Self {
            active,
            alternate: None,
        }
    }

    /// Makes `kind` the active backend. `false` when it is neither active nor the alternate.
    pub fn switch_to(&mut self, kind: BackendKind) -> bool {
        if self.active.info.kind == kind {
            return true;
        }
        match self.alternate.take() {
            Some(alternate) if alternate.info.kind == kind => {
                self.alternate = Some(std::mem::replace(&mut self.active, alternate));
                true
            }
            alternate => {
                self.alternate = alternate;
                false
            }
        }
    }
}

/// A change the popup asks to persist in `config.toml`.
#[derive(Debug, Clone, PartialEq)]
pub enum Persist {
    /// The target and, with it, the most recent target.
    Target(String),
    /// The most recent source language.
    Source(String),
    /// The backend chosen with the popup's switch.
    Backend(BackendKind),
    Splitter(f32),
    Size(Size),
}

/// Popup visibility and the per-activation request.
pub struct Popups<U: Ui> {
    ui: U,
    capture: Option<Arc<dyn Capture>>,
    backends: Backends,
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
        backends: Backends,
        config: Config,
        theme: Theme,
    ) -> Self {
        Self {
            ui,
            capture,
            backends,
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
            translator: self.backends.active.translator.clone(),
            alternate_translator: self
                .backends
                .alternate
                .as_ref()
                .map(|alternate| alternate.translator.clone()),
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
        if self.backends.active.info.configured {
            let translator = self.backends.active.translator.clone();
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
                self.config.remember_target(&code);
                Some(Persist::Target(code))
            }
            ShellEvent::RememberSource(code) => {
                self.config.remember_source(&code);
                Some(Persist::Source(code))
            }
            ShellEvent::BackendSwitched(kind) => {
                if !self.backends.switch_to(kind) {
                    // The backends were rebuilt meanwhile (settings changed) without this one.
                    tracing::debug!(backend = ?kind, "ignoring a switch to a backend that is gone");
                    return None;
                }
                tracing::debug!(backend = ?kind, "backend switched from the popup");
                self.config.backend = kind;
                Some(Persist::Backend(kind))
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

    pub fn set_backends(&mut self, backends: Backends) {
        self.backends = backends;
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
            recent_targets: to_owned(self.config.recent_targets()),
            recent_sources: to_owned(self.config.recent_sources()),
            languages: Languages::deepl(),
            backend: self.backends.active.info.clone(),
            alternate: self
                .backends
                .alternate
                .as_ref()
                .map(|alternate| alternate.info.clone()),
            splitter: self.config.popup.splitter,
            theme: self.theme.clone(),
        }
    }
}

fn to_owned(codes: Vec<&'static str>) -> Vec<String> {
    codes.into_iter().map(str::to_owned).collect()
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
                kind: BackendKind::Deepl,
                label: "DeepL".into(),
                configured: true,
                reports_detected: true,
            },
        };
        let popups = Popups::new(
            ui.clone(),
            capture,
            Backends::single(backend),
            Config::default(),
            Theme::Light,
        );
        (popups, ui, translator)
    }

    fn backend(kind: BackendKind, translator: Arc<dyn Translator>) -> Backend {
        Backend {
            translator,
            info: BackendInfo {
                kind,
                label: match kind {
                    BackendKind::Deepl => "DeepL".into(),
                    BackendKind::Openai => "AI: test".into(),
                },
                configured: true,
                reports_detected: kind == BackendKind::Deepl,
            },
        }
    }

    /// DeepL active with AI as the alternate, each with its own translator.
    fn switchable() -> (
        Popups<FakeUi>,
        FakeUi,
        Arc<HangingTranslator>,
        Arc<HangingTranslator>,
    ) {
        let ui = FakeUi::default();
        let deepl = Arc::new(HangingTranslator::default());
        let ai = Arc::new(HangingTranslator::default());
        let backends = Backends {
            active: backend(BackendKind::Deepl, deepl.clone()),
            alternate: Some(backend(BackendKind::Openai, ai.clone())),
        };
        let popups = Popups::new(ui.clone(), None, backends, Config::default(), Theme::Light);
        (popups, ui, deepl, ai)
    }

    fn same(a: &Arc<dyn Translator>, b: &Arc<HangingTranslator>) -> bool {
        std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b))
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
    async fn without_a_second_backend_the_popup_gets_no_switch() {
        let (mut popups, ui, _) = popups(None);
        popups.toggle(None, Instant::now());
        let request = ui.last_show().expect("show");
        assert!(request.open.alternate.is_none());
        assert!(request.alternate_translator.is_none());
        // A switch event without an alternate changes nothing and persists nothing.
        assert_eq!(
            popups.shell_event(ShellEvent::BackendSwitched(BackendKind::Openai)),
            None
        );
        assert_eq!(popups.config().backend, BackendKind::Deepl);
    }

    #[tokio::test]
    async fn both_backends_reach_the_popup_and_the_switch_sticks() {
        let (mut popups, ui, deepl, ai) = switchable();
        popups.toggle(None, Instant::now());
        let request = ui.last_show().expect("show");
        assert_eq!(request.open.backend.kind, BackendKind::Deepl);
        assert_eq!(
            request.open.alternate.as_ref().map(|b| b.kind),
            Some(BackendKind::Openai)
        );
        assert!(same(&request.translator, &deepl));
        assert!(same(
            request.alternate_translator.as_ref().expect("alternate"),
            &ai
        ));

        assert_eq!(
            popups.shell_event(ShellEvent::BackendSwitched(BackendKind::Openai)),
            Some(Persist::Backend(BackendKind::Openai))
        );
        assert_eq!(popups.config().backend, BackendKind::Openai);
        popups.hide();
        popups.toggle(None, Instant::now());
        let request = ui.last_show().expect("show");
        assert_eq!(request.open.backend.kind, BackendKind::Openai);
        assert_eq!(
            request.open.alternate.as_ref().map(|b| b.kind),
            Some(BackendKind::Deepl)
        );
        assert!(same(&request.translator, &ai));
        tokio::task::yield_now().await;
        assert_eq!(
            ai.warm_ups.load(Ordering::SeqCst),
            1,
            "warms the new backend"
        );

        // Switching back, and a repeated event for the active backend, keep things consistent.
        assert_eq!(
            popups.shell_event(ShellEvent::BackendSwitched(BackendKind::Deepl)),
            Some(Persist::Backend(BackendKind::Deepl))
        );
        assert_eq!(
            popups.shell_event(ShellEvent::BackendSwitched(BackendKind::Deepl)),
            Some(Persist::Backend(BackendKind::Deepl))
        );
        assert_eq!(popups.config().backend, BackendKind::Deepl);
    }

    #[tokio::test]
    async fn a_stale_switch_after_a_rebuild_is_ignored() {
        let (mut popups, _ui, _deepl, _ai) = switchable();
        // Settings removed the AI key meanwhile: only DeepL is left.
        popups.set_backends(Backends::single(backend(
            BackendKind::Deepl,
            Arc::new(HangingTranslator::default()),
        )));
        assert_eq!(
            popups.shell_event(ShellEvent::BackendSwitched(BackendKind::Openai)),
            None
        );
        assert_eq!(popups.config().backend, BackendKind::Deepl);
    }

    #[tokio::test]
    async fn picked_languages_become_recent_for_the_next_popup() {
        let (mut popups, ui, _) = popups(None);
        for code in ["DE", "FR", "TR", "FR"] {
            assert_eq!(
                popups.shell_event(ShellEvent::PersistTarget(code.into())),
                Some(Persist::Target(code.into()))
            );
        }
        assert_eq!(
            popups.shell_event(ShellEvent::RememberSource("EN".into())),
            Some(Persist::Source("EN".into()))
        );
        popups.toggle(None, Instant::now());
        let request = ui.last_show().expect("show");
        assert_eq!(request.open.target_lang, "FR");
        assert_eq!(request.open.recent_targets, ["FR", "TR", "DE"]);
        assert_eq!(request.open.recent_sources, ["EN"]);
    }

    #[test]
    fn backends_switch_to() {
        let deepl = backend(BackendKind::Deepl, Arc::new(HangingTranslator::default()));
        let ai = backend(BackendKind::Openai, Arc::new(HangingTranslator::default()));
        let mut single = Backends::single(deepl.clone());
        assert!(single.switch_to(BackendKind::Deepl));
        assert!(!single.switch_to(BackendKind::Openai));
        assert_eq!(single.active.info.kind, BackendKind::Deepl);
        assert!(single.alternate.is_none());

        let mut both = Backends {
            active: deepl,
            alternate: Some(ai),
        };
        assert!(both.switch_to(BackendKind::Openai));
        assert_eq!(both.active.info.kind, BackendKind::Openai);
        assert_eq!(
            both.alternate.as_ref().map(|b| b.info.kind),
            Some(BackendKind::Deepl)
        );
        assert!(both.switch_to(BackendKind::Openai));
        assert_eq!(both.active.info.kind, BackendKind::Openai);
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
