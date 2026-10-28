//! Shell-independent popup hosting: owns the [`Popup`], runs its effects and reports to the
//! daemon. The layer and window shells only add surfaces, placement and the clipboard.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use iced::{Element, Task, Theme};
use quick_translate_backends::Translator;
use tokio_util::sync::CancellationToken;

use crate::popup::{
    Control, Effect, Message, OpenOptions, Pane, Popup, TranslationId, focus_task, translation_task,
};
use crate::shell::{SessionId, ShellEnd, ShellEvent, ShowRequest, elapsed_ms};

/// What the shell must do after an update, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostAction {
    /// Put the text on CLIPBOARD and keep serving it after the popup closes.
    Copy(String),
    /// Tear the popup surfaces down, then call [`PopupHost::closed`].
    Close,
}

/// The popup of the current session, if any.
pub struct PopupHost {
    shell: ShellEnd,
    popup: Option<Popup>,
    session: Option<SessionId>,
    translator: Option<Arc<dyn Translator>>,
    cancels: HashMap<TranslationId, CancellationToken>,
    theme: Theme,
    triggered_at: Option<Instant>,
    first_frame_logged: bool,
}

impl PopupHost {
    pub fn new(shell: ShellEnd, theme: Theme) -> Self {
        Self {
            shell,
            popup: None,
            session: None,
            translator: None,
            cancels: HashMap::new(),
            theme,
            triggered_at: None,
            first_frame_logged: false,
        }
    }

    pub fn shell(&self) -> &ShellEnd {
        &self.shell
    }

    /// The session whose popup is open.
    pub fn session(&self) -> Option<SessionId> {
        self.session
    }

    pub fn is_open(&self) -> bool {
        self.session.is_some()
    }

    pub fn theme(&self) -> Theme {
        self.theme.clone()
    }

    pub fn popup(&self) -> Option<&Popup> {
        self.popup.as_ref()
    }

    /// Starts a session. The caller has already torn down any previous popup surfaces.
    pub fn open(&mut self, request: &ShowRequest) -> Task<Message> {
        self.cancel_all();
        let mut open: OpenOptions = request.open.clone();
        open.theme = self.theme.clone();
        let effects = match &mut self.popup {
            Some(popup) => popup.reset(open),
            None => {
                self.popup = Some(Popup::new(open));
                Vec::new()
            }
        };
        if let Some(popup) = &mut self.popup {
            popup.set_notice(request.notice.clone());
        }
        self.session = Some(request.session);
        self.translator = Some(request.translator.clone());
        self.triggered_at = Some(request.triggered_at);
        self.first_frame_logged = false;
        let (task, _) = self.run(effects);
        task
    }

    /// Logs the first frame of the session once (hot-path instrumentation, task 8.7).
    pub fn log_first_frame(&mut self, what: &str) {
        if self.first_frame_logged {
            return;
        }
        self.first_frame_logged = true;
        if let (Some(session), Some(at)) = (self.session, self.triggered_at) {
            tracing::debug!(
                session = session.0,
                ms = format!("{:.1}", elapsed_ms(at)),
                "hot path: popup first frame ({what})"
            );
        }
    }

    /// Gives toolkit focus to the pane the popup considers focused. Call when the panel first
    /// appears: a focus request made before the panel existed (fast capture) found no widget.
    pub fn refocus(&self) -> Task<Message> {
        let pane = match self.popup.as_ref().and_then(Popup::focused) {
            Some(Control::SourcePane) => Some(Pane::Source),
            Some(Control::TranslationPane) => Some(Pane::Translation),
            _ => return Task::none(),
        };
        focus_task(pane)
    }

    /// Feeds a finished capture into the popup when it belongs to the open session.
    pub fn captured(
        &mut self,
        session: SessionId,
        text: String,
        truncated: bool,
    ) -> (Task<Message>, Vec<HostAction>) {
        if self.session != Some(session) {
            return (Task::none(), Vec::new());
        }
        if let Some(at) = self.triggered_at {
            tracing::debug!(
                session = session.0,
                ms = format!("{:.1}", elapsed_ms(at)),
                chars = text.chars().count(),
                truncated,
                "hot path: capture done"
            );
        }
        self.update(Message::Captured { text, truncated })
    }

    /// Processes a popup message for the open session.
    pub fn update(&mut self, message: Message) -> (Task<Message>, Vec<HostAction>) {
        if let Message::TranslationFinished { id } | Message::TranslationFailed { id, .. } =
            &message
        {
            self.cancels.remove(id);
        }
        let Some(popup) = &mut self.popup else {
            return (Task::none(), Vec::new());
        };
        if self.session.is_none() {
            return (Task::none(), Vec::new());
        }
        let effects = popup.update(message);
        self.run(effects)
    }

    /// Asks the popup to close (outside click, focus loss, toggle). Cancels the translation.
    pub fn request_close(&mut self) -> (Task<Message>, Vec<HostAction>) {
        self.update(Message::CloseRequested)
    }

    /// The shell removed the popup surfaces: report it and forget the session.
    pub fn closed(&mut self) {
        self.cancel_all();
        if let Some(session) = self.session.take() {
            tracing::debug!(session = session.0, "popup closed");
            self.shell.send(ShellEvent::Closed { session });
        }
        self.triggered_at = None;
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme.clone();
        if let Some(popup) = &mut self.popup {
            popup.set_theme(theme);
        }
    }

    /// The panel of the open session.
    pub fn view(&self) -> Option<Element<'_, Message>> {
        self.session?;
        self.popup.as_ref().map(Popup::view)
    }

    fn cancel_all(&mut self) {
        for (_, token) in self.cancels.drain() {
            token.cancel();
        }
    }

    fn run(&mut self, effects: Vec<Effect>) -> (Task<Message>, Vec<HostAction>) {
        let mut tasks = Vec::new();
        let mut actions = Vec::new();
        for effect in effects {
            let effect = match effect.into_task() {
                Ok(task) => {
                    tasks.push(task);
                    continue;
                }
                Err(effect) => effect,
            };
            match effect {
                Effect::StartTranslation { id, request } => {
                    let Some(translator) = self.translator.clone() else {
                        continue;
                    };
                    let cancel = CancellationToken::new();
                    self.cancels.insert(id, cancel.clone());
                    tasks.push(translation_task(translator, id, request, cancel));
                }
                Effect::CancelTranslation(id) => {
                    if let Some(token) = self.cancels.remove(&id) {
                        token.cancel();
                    }
                }
                Effect::CopyToClipboard(text) => actions.push(HostAction::Copy(text)),
                Effect::Close => actions.push(HostAction::Close),
                Effect::OpenSettings => self.shell.send(ShellEvent::OpenSettings),
                Effect::PersistTarget(code) => self.shell.send(ShellEvent::PersistTarget(code)),
                Effect::PersistSplitter(ratio) => {
                    self.shell.send(ShellEvent::PersistSplitter(ratio));
                }
                Effect::ScheduleDebounce { .. } | Effect::Focus(_) => {}
            }
        }
        (Task::batch(tasks), actions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::popup::{BackendInfo, Languages};
    use crate::shell::{CaptureMode, channel};
    use iced::Size;
    use quick_translate_backends::{ChunkStream, Request};
    use std::pin::Pin;

    struct Never;

    impl Translator for Never {
        fn translate(
            &self,
            _req: Request,
            _cancel: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = quick_translate_backends::Result<ChunkStream>> + Send + '_>>
        {
            Box::pin(std::future::pending())
        }
    }

    fn request(session: u64) -> ShowRequest {
        ShowRequest {
            session: SessionId(session),
            activation_token: None,
            open: OpenOptions {
                target_lang: "DE".into(),
                languages: Languages::deepl(),
                backend: BackendInfo {
                    label: "DeepL".into(),
                    configured: true,
                    reports_detected: true,
                },
                splitter: 0.5,
                theme: Theme::Light,
            },
            size: Size::new(720.0, 320.0),
            translator: Arc::new(Never),
            capture: CaptureMode::Daemon,
            notice: Some("config.toml is malformed".into()),
            triggered_at: Instant::now(),
        }
    }

    #[test]
    fn capture_of_another_session_is_ignored() {
        let (_daemon, shell) = channel();
        let mut host = PopupHost::new(shell, Theme::Dark);
        let _ = host.open(&request(1));
        let _ = host.captured(SessionId(0), "stale".into(), false);
        assert!(host.popup().is_some_and(|p| p.is_capture_pending()));
        let _ = host.captured(SessionId(1), "Hallo".into(), false);
        let popup = host.popup().expect("popup");
        assert_eq!(popup.source_text().trim_end(), "Hallo");
        assert_eq!(popup.notice(), Some("config.toml is malformed"));
        assert_eq!(popup.theme(), Theme::Dark);
        assert_eq!(host.cancels.len(), 1, "translation started");
    }

    #[test]
    fn close_cancels_and_reports_once() {
        let (mut daemon, shell) = channel();
        let mut host = PopupHost::new(shell, Theme::Light);
        let _ = host.open(&request(7));
        let _ = host.captured(SessionId(7), "Hallo".into(), false);
        let token = host.cancels.values().next().cloned().expect("token");
        let (_, actions) = host.request_close();
        assert_eq!(actions, vec![HostAction::Close]);
        assert!(token.is_cancelled());
        host.closed();
        host.closed();
        assert_eq!(
            daemon.events.try_recv().ok(),
            Some(ShellEvent::Closed {
                session: SessionId(7)
            })
        );
        assert!(daemon.events.try_recv().is_err());
        assert!(!host.is_open());
    }
}
