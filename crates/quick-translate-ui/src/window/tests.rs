use std::pin::Pin;

use quick_translate_backends::{ChunkStream, Request, Translator};
use tokio::sync::mpsc::error::TryRecvError;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::popup::{BackendInfo, Languages, OpenOptions};
use crate::shell::{DaemonEnd, channel};

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

fn request(session: u64, capture: CaptureMode) -> ShowRequest {
    ShowRequest {
        session: SessionId(session),
        activation_token: Some("token".into()),
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
        capture,
        notice: None,
        triggered_at: Instant::now(),
    }
}

fn shell() -> (Shell, DaemonEnd) {
    let (daemon, end) = channel();
    (Shell::new(end, Theme::Dark), daemon)
}

fn events(daemon: &mut DaemonEnd) -> Vec<ShellEvent> {
    let mut events = Vec::new();
    loop {
        match daemon.events.try_recv() {
            Ok(event) => events.push(event),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return events,
        }
    }
}

impl Shell {
    /// Shows a popup; a new window also reports `Opened`.
    fn show_and_open(&mut self, session: u64, capture: CaptureMode) -> window::Id {
        let before = self.popup.as_ref().map(|p| p.id);
        let _ = self.update(Message::Command(ShellCommand::Show(Box::new(request(
            session, capture,
        )))));
        let popup = self.popup.as_ref().expect("window");
        let (id, size) = (popup.id, popup.requested);
        if before != Some(id) {
            let _ = self.window(
                id,
                window::Event::Opened {
                    position: None,
                    size,
                },
            );
        }
        id
    }

    fn window(&mut self, id: window::Id, event: window::Event) -> Task<Message> {
        self.update(Message::Window(id, event))
    }

    fn has_session(&self) -> bool {
        self.popup.as_ref().is_some_and(|p| p.session.is_some())
    }

    fn capturing(&self) -> bool {
        self.popup
            .as_ref()
            .and_then(|p| p.session.as_ref())
            .is_some_and(|s| s.capturing)
    }
}

fn shown(session: u64) -> ShellEvent {
    ShellEvent::Shown {
        session: SessionId(session),
    }
}

fn closed(session: u64) -> ShellEvent {
    ShellEvent::Closed {
        session: SessionId(session),
    }
}

#[tokio::test]
async fn opened_window_reports_shown() {
    let (mut shell, mut daemon) = shell();
    shell.show_and_open(1, CaptureMode::Daemon);
    assert_eq!(events(&mut daemon), vec![shown(1)]);
    assert!(shell.host.is_open());
}

#[tokio::test]
async fn focus_loss_after_focus_closes() {
    let (mut shell, mut daemon) = shell();
    let id = shell.show_and_open(1, CaptureMode::Daemon);
    let _ = shell.window(id, window::Event::Unfocused);
    assert!(shell.has_session(), "never focused: keep it");
    let _ = shell.window(id, window::Event::Focused);
    let _ = shell.window(id, window::Event::Unfocused);
    assert!(
        shell.popup.is_none(),
        "focused before: the window goes at once"
    );
    assert_eq!(events(&mut daemon), vec![shown(1), closed(1)]);
}

#[tokio::test]
async fn resize_then_close_persists_size_once() {
    let (mut shell, mut daemon) = shell();
    let id = shell.show_and_open(1, CaptureMode::Daemon);
    let _ = shell.window(id, window::Event::Focused);
    let _ = shell.window(id, window::Event::Resized(Size::new(800.0, 410.0)));
    let _ = shell.window(id, window::Event::CloseRequested);
    let _ = shell.update(Message::Command(ShellCommand::Hide));
    let _ = shell.window(id, window::Event::Closed);
    assert_eq!(
        events(&mut daemon),
        vec![
            shown(1),
            ShellEvent::PersistSize(Size::new(800.0, 410.0)),
            closed(1),
        ]
    );
}

#[tokio::test]
async fn unchanged_size_is_not_persisted() {
    let (mut shell, mut daemon) = shell();
    let id = shell.show_and_open(1, CaptureMode::Daemon);
    let _ = shell.window(id, window::Event::Resized(Size::new(720.2, 320.0)));
    let _ = shell.update(Message::Command(ShellCommand::Hide));
    assert!(
        !events(&mut daemon)
            .iter()
            .any(|e| matches!(e, ShellEvent::PersistSize(_)))
    );
}

#[tokio::test]
async fn show_replaces_a_focused_popup_with_a_new_window() {
    let (mut shell, mut daemon) = shell();
    let first = shell.show_and_open(1, CaptureMode::Daemon);
    let _ = shell.window(first, window::Event::Focused);
    let second = shell.show_and_open(2, CaptureMode::Daemon);
    assert_ne!(first, second);
    // Late events of the old window change nothing.
    let _ = shell.window(first, window::Event::Unfocused);
    let _ = shell.window(first, window::Event::Closed);
    assert_eq!(shell.host.session(), Some(SessionId(2)));
    assert_eq!(events(&mut daemon), vec![shown(1), closed(1), shown(2)]);
}

#[tokio::test]
async fn show_reuses_a_window_whose_focus_has_not_settled() {
    let (mut shell, mut daemon) = shell();
    let first = shell.show_and_open(1, CaptureMode::Daemon);
    let second = shell.show_and_open(2, CaptureMode::Daemon);
    assert_eq!(first, second);
    assert_eq!(events(&mut daemon), vec![shown(1), closed(1), shown(2)]);
}

#[tokio::test]
async fn toggling_show_and_hide_repeatedly() {
    let (mut shell, mut daemon) = shell();
    for session in 1..=5 {
        shell.show_and_open(session, CaptureMode::Focus);
        let _ = shell.update(Message::Command(ShellCommand::Hide));
        assert!(!shell.has_session());
        assert_eq!(events(&mut daemon), vec![shown(session), closed(session)]);
    }
}

#[tokio::test]
async fn hidden_popup_keeps_its_window_until_focus_settles() {
    let (mut shell, mut daemon) = shell();
    let id = shell.show_and_open(1, CaptureMode::Focus);
    let _ = shell.update(Message::Command(ShellCommand::Hide));
    assert_eq!(events(&mut daemon), vec![shown(1), closed(1)]);
    assert!(shell.popup.is_some() && !shell.has_session());
    let _ = shell.window(id, window::Event::RedrawRequested(Instant::now()));
    let _ = shell.window(id, window::Event::Focused);
    assert!(shell.popup.is_none());
    assert!(events(&mut daemon).is_empty());

    let id = shell.show_and_open(2, CaptureMode::Focus);
    let _ = shell.update(Message::Command(ShellCommand::Hide));
    let _ = shell.update(Message::FocusWaitOver(id));
    assert!(shell.popup.is_none());

    let id = shell.show_and_open(3, CaptureMode::Focus);
    let _ = shell.update(Message::Command(ShellCommand::Hide));
    let _ = shell.update(Message::CloseWindow(id));
    assert!(
        shell.popup.is_none(),
        "closed after the linger limit at the latest"
    );
}

#[tokio::test]
async fn escape_closes_at_once() {
    let (mut shell, mut daemon) = shell();
    shell.show_and_open(1, CaptureMode::Daemon);
    let _ = shell.update(Message::Popup(popup::Message::Key(popup::Key::Escape)));
    assert!(!shell.has_session());
    assert_eq!(events(&mut daemon).last(), Some(&closed(1)));
}

#[tokio::test]
async fn focus_capture_waits_for_focus_and_display() {
    let (mut shell, _daemon) = shell();
    let id = shell.show_and_open(1, CaptureMode::Focus);
    let _ = shell.window(id, window::Event::Focused);
    assert!(!shell.capturing(), "no clipboard yet");
    let _ = shell.update(Message::Display(None));
    assert!(shell.capturing());
    assert!(matches!(shell.activation, Activation::Unavailable));
}

#[tokio::test]
async fn focus_wait_over_starts_capture_without_focus() {
    let (mut shell, _daemon) = shell();
    let _ = shell.update(Message::Display(None));
    let id = shell.show_and_open(1, CaptureMode::Focus);
    assert!(!shell.capturing());
    let _ = shell.update(Message::FocusWaitOver(id));
    assert!(shell.capturing());
}

#[tokio::test]
async fn reused_window_captures_for_the_new_session() {
    let (mut shell, _daemon) = shell();
    let _ = shell.update(Message::Display(None));
    let id = shell.show_and_open(1, CaptureMode::Focus);
    let _ = shell.update(Message::Command(ShellCommand::Hide));
    let _ = shell.update(Message::FocusWaitOver(id));
    assert!(shell.popup.is_none());
    let id = shell.show_and_open(2, CaptureMode::Focus);
    let _ = shell.update(Message::Command(ShellCommand::Hide));
    assert!(!shell.capturing());
    let again = shell.show_and_open(3, CaptureMode::Focus);
    assert_eq!(id, again);
    let _ = shell.window(id, window::Event::Focused);
    assert!(shell.capturing());
}

#[tokio::test]
async fn daemon_capture_never_reads_the_clipboard() {
    let (mut shell, _daemon) = shell();
    let _ = shell.update(Message::Display(None));
    let id = shell.show_and_open(1, CaptureMode::Daemon);
    let _ = shell.window(id, window::Event::Focused);
    assert!(!shell.capturing());
    let _ = shell.update(Message::Command(ShellCommand::Captured {
        session: SessionId(1),
        text: "Hallo".into(),
        truncated: false,
    }));
    let popup = shell.host.popup().expect("popup");
    assert_eq!(popup.source_text().trim_end(), "Hallo");
}

#[tokio::test]
async fn stale_capture_is_ignored() {
    let (mut shell, _daemon) = shell();
    shell.show_and_open(2, CaptureMode::Focus);
    let stale = Captured {
        text: "old".into(),
        truncated: false,
        origin: selection::Origin::Clipboard,
    };
    let _ = shell.update(Message::Captured(SessionId(1), stale));
    assert!(shell.host.popup().expect("popup").is_capture_pending());
}

#[tokio::test]
async fn quit_ends_the_session() {
    let (mut shell, mut daemon) = shell();
    shell.show_and_open(3, CaptureMode::Daemon);
    let _ = shell.update(Message::Command(ShellCommand::Quit));
    assert!(!shell.has_session());
    assert_eq!(events(&mut daemon).last(), Some(&closed(3)));
}

#[test]
fn helpers() {
    assert_eq!(initial_size(Size::new(100.0, 50.0)), MIN_SIZE);
    assert_eq!(
        initial_size(Size::new(900.0, 400.0)),
        Size::new(900.0, 400.0)
    );
    assert_eq!(
        initial_size(Size::new(f32::NAN, 400.0)).width,
        MIN_SIZE.width
    );
    let requested = Size::new(720.0, 320.0);
    assert_eq!(persisted_size(requested, requested), None);
    assert_eq!(persisted_size(requested, Size::new(720.5, 320.0)), None);
    assert_eq!(
        persisted_size(requested, Size::new(720.0, 300.0)),
        Some(Size::new(720.0, 300.0))
    );
    assert_eq!(persisted_size(requested, Size::ZERO), None);
    assert_eq!(capture_budget(true), STEP_TIMEOUT);
    assert_eq!(capture_budget(false), Duration::ZERO);
}
