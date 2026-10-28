//! Window shell for compositors without layer-shell, e.g. GNOME (design D4, D5).
//!
//! Each popup is a normal xdg_toplevel that the compositor places. With
//! [`CaptureMode::Focus`] the selection is read through the toolkit clipboard once the window has
//! keyboard focus, after activating it with the trigger's xdg-activation token.
//!
//! At most one window exists at a time, and it is only destroyed once its first keyboard focus
//! has settled (it got focus, or the focus wait is over). iced drops its per-window smithay
//! clipboard when a window closes, releasing that clipboard's data device. If the compositor
//! focuses one of our windows at that moment, it sends a selection offer to the released device,
//! libwayland loses the offer's new object id and the next offer kills the connection ("not a
//! valid new object id"). A popup hidden before that point keeps its window until focus settles,
//! and a popup shown meanwhile reuses it.

pub(crate) mod raw;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::space;
use iced::{Element, Size, Subscription, Task, Theme, event, window};
use quick_translate_platform::selection::{self, Captured, STEP_TIMEOUT};

use crate::clipboard::FocusClipboard;
use crate::executor::SharedExecutor;
use crate::host::{HostAction, PopupHost};
use crate::layer::geometry::fit;
use crate::popup;
use crate::shell::{
    CaptureMode, Inbox, MIN_SIZE, SessionId, ShellCommand, ShellEnd, ShellEvent, ShowRequest,
    elapsed_ms,
};

use raw::{Activator, DisplayPtr};

/// How long capture waits for keyboard focus after the window maps before reading without it.
const FOCUS_WAIT: Duration = Duration::from_millis(150);
/// Time for a CLIPBOARD store to reach the compositor while the window still has focus.
const COPY_SETTLE: Duration = Duration::from_millis(40);
/// Upper bound for keeping a hidden popup's window until its first focus settles.
const LINGER_MAX: Duration = Duration::from_secs(1);
/// Size changes below this (logical px) are not persisted.
const SIZE_EPSILON: f32 = 1.0;

/// Settings of the window shell.
pub struct WindowShell {
    pub shell: ShellEnd,
    pub theme: Theme,
}

/// Runs the window shell until [`ShellCommand::Quit`]. Blocks the calling (main) thread.
pub fn run(settings: WindowShell) -> iced::Result {
    let WindowShell { shell, theme } = settings;
    iced::daemon(
        move || (Shell::new(shell.clone(), theme.clone()), Task::none()),
        Shell::update,
        Shell::view,
    )
    .title(|_: &Shell, _| String::from("Quick Translate"))
    .theme(|shell: &Shell, _| shell.host.theme())
    .subscription(Shell::subscription)
    .executor::<SharedExecutor>()
    .run()
}

#[derive(Debug, Clone)]
enum Message {
    Command(ShellCommand),
    Popup(popup::Message),
    Window(window::Id, window::Event),
    Display(Option<DisplayPtr>),
    Activator(Option<Arc<Activator>>),
    FocusWaitOver(window::Id),
    Captured(SessionId, Captured),
    /// Close the window now if it holds no session (or the given session's close is due).
    CloseWindow(window::Id),
}

/// The popup window.
#[derive(Debug)]
struct Popup {
    id: window::Id,
    /// The size the current session asked for, to detect user resizes.
    requested: Size,
    size: Size,
    opened: bool,
    drawn: bool,
    focused: bool,
    had_focus: bool,
    focus_wait_over: bool,
    /// `None` while a hidden popup waits for its focus to settle.
    session: Option<Session>,
}

impl Popup {
    fn settled(&self) -> bool {
        self.had_focus || self.focus_wait_over
    }
}

#[derive(Debug)]
struct Session {
    id: SessionId,
    capture: CaptureMode,
    token: Option<String>,
    triggered_at: Instant,
    capturing: bool,
    /// A delayed close is scheduled; further close requests are redundant.
    closing: bool,
}

#[derive(Debug)]
enum Activation {
    /// No window has reported its display yet.
    Unknown,
    Binding,
    Ready(Arc<Activator>),
    Unavailable,
}

struct Shell {
    host: PopupHost,
    inbox: Inbox,
    popup: Option<Popup>,
    /// The display has been looked up (it may not be Wayland).
    display_known: bool,
    clipboard: Option<FocusClipboard>,
    activation: Activation,
}

impl Shell {
    fn new(shell: ShellEnd, theme: Theme) -> Self {
        Self {
            inbox: shell.commands.clone(),
            host: PopupHost::new(shell, theme),
            popup: None,
            display_known: false,
            clipboard: None,
            activation: Activation::Unknown,
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        // Redraws only matter until the first frame: listening longer would wake every frame.
        let first_frame = if self.popup.as_ref().is_some_and(|p| !p.drawn) {
            event::listen_raw(|event, _, id| match event {
                iced::Event::Window(event @ window::Event::RedrawRequested(_)) => {
                    Some(Message::Window(id, event))
                }
                _ => None,
            })
        } else {
            Subscription::none()
        };
        Subscription::batch([
            self.inbox.subscription().map(Message::Command),
            window::events().map(|(id, event)| Message::Window(id, event)),
            popup::subscription().map(Message::Popup),
            first_frame,
        ])
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        if self
            .popup
            .as_ref()
            .is_some_and(|p| p.id == id && p.session.is_some())
            && let Some(panel) = self.host.view()
        {
            return panel.map(Message::Popup);
        }
        space().into()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Command(command) => self.command(command),
            Message::Popup(message) => {
                let (task, actions) = self.host.update(message);
                Task::batch([task.map(Message::Popup), self.act(actions)])
            }
            Message::Window(id, event) => self.window_event(id, event),
            Message::Display(display) => self.display(display),
            Message::Activator(activator) => {
                self.activation = match activator {
                    Some(activator) => Activation::Ready(activator),
                    None => {
                        tracing::debug!("compositor has no xdg_activation_v1");
                        Activation::Unavailable
                    }
                };
                self.activate()
            }
            Message::FocusWaitOver(id) => {
                let Some(popup) = self.popup_mut(id) else {
                    return Task::none();
                };
                popup.focus_wait_over = true;
                match &popup.session {
                    None => self.drop_window(),
                    Some(session) => {
                        if !popup.had_focus {
                            tracing::debug!(session = session.id.0, "popup got no keyboard focus");
                        }
                        self.start_capture()
                    }
                }
            }
            Message::Captured(session, captured) => {
                let (task, actions) =
                    self.host
                        .captured(session, captured.text, captured.truncated);
                Task::batch([task.map(Message::Popup), self.act(actions)])
            }
            Message::CloseWindow(id) => match self.popup_mut(id) {
                Some(popup) if popup.session.is_none() => self.drop_window(),
                Some(popup) if popup.session.as_ref().is_some_and(|s| s.closing) => {
                    self.end_session()
                }
                _ => Task::none(),
            },
        }
    }

    fn command(&mut self, command: ShellCommand) -> Task<Message> {
        match command {
            ShellCommand::Show(request) => self.show(&request),
            ShellCommand::Hide => self.request_close(),
            ShellCommand::Captured {
                session,
                text,
                truncated,
            } => {
                let (task, actions) = self.host.captured(session, text, truncated);
                Task::batch([task.map(Message::Popup), self.act(actions)])
            }
            ShellCommand::SetTheme(theme) => {
                self.host.set_theme(theme);
                Task::none()
            }
            ShellCommand::Quit => {
                // A lingering window goes down with the event loop.
                let close = self.end_session();
                Task::batch([close, iced::exit()])
            }
        }
    }

    fn show(&mut self, request: &ShowRequest) -> Task<Message> {
        let replaced = self.end_session();
        let open = self.host.open(request).map(Message::Popup);
        let size = initial_size(request.size);
        tracing::debug!(
            session = request.session.0,
            ms = format!("{:.1}", elapsed_ms(request.triggered_at)),
            capture = ?request.capture,
            token = request.activation_token.is_some(),
            reuse = self.popup.is_some(),
            "window shell: showing popup"
        );
        let session = Session {
            id: request.session,
            capture: request.capture,
            token: request.activation_token.clone(),
            triggered_at: request.triggered_at,
            capturing: false,
            closing: false,
        };
        if let Some(popup) = &mut self.popup {
            // The previous window has not settled yet: show the new session in it.
            popup.session = Some(session);
            popup.requested = size;
            let resize = if (popup.size.width - size.width).abs() >= SIZE_EPSILON
                || (popup.size.height - size.height).abs() >= SIZE_EPSILON
            {
                window::resize(popup.id, size)
            } else {
                Task::none()
            };
            let shown = if popup.opened {
                self.shown()
            } else {
                Task::none()
            };
            return Task::batch([
                replaced,
                open,
                resize,
                shown,
                self.activate(),
                self.start_capture(),
            ]);
        }
        let (id, opened) = window::open(window::Settings {
            size,
            min_size: Some(MIN_SIZE),
            resizable: true,
            decorations: true,
            exit_on_close_request: false,
            platform_specific: window::settings::PlatformSpecific {
                application_id: quick_translate_core::APP_ID.to_owned(),
                ..Default::default()
            },
            ..window::Settings::default()
        });
        self.popup = Some(Popup {
            id,
            requested: size,
            size,
            opened: false,
            drawn: false,
            focused: false,
            had_focus: false,
            focus_wait_over: false,
            session: Some(session),
        });
        // The toolkit clipboard must exist before the window maps: some compositors (weston,
        // likely Mutter) send the selection to a data device only on focus changes, so one
        // created after focus sees no offer until the next popup.
        let display = if self.display_known {
            opened.discard()
        } else {
            opened.then(|id| window::run(id, DisplayPtr::from_window).map(Message::Display))
        };
        Task::batch([replaced, open, display])
    }

    /// Reports the open session as visible and looks up the display once.
    fn shown(&mut self) -> Task<Message> {
        let Some(popup) = self.popup.as_ref().filter(|p| p.session.is_some()) else {
            return Task::none();
        };
        let id = popup.id;
        self.host.log_first_frame("window opened");
        if let Some(session) = self.host.session() {
            self.host.shell().send(ShellEvent::Shown { session });
        }
        // A focus request made before the panel existed found no widget.
        let refocus = self.host.refocus().map(Message::Popup);
        if self.display_known {
            refocus
        } else {
            Task::batch([
                refocus,
                window::run(id, DisplayPtr::from_window).map(Message::Display),
            ])
        }
    }

    fn window_event(&mut self, id: window::Id, event: window::Event) -> Task<Message> {
        let Some(popup) = self.popup_mut(id) else {
            return Task::none();
        };
        let lingering = popup.session.is_none();
        match event {
            window::Event::Opened { size, .. } => {
                popup.opened = true;
                popup.size = size;
                Task::batch([self.shown(), self.start_capture()])
            }
            window::Event::RedrawRequested(_) => {
                if popup.drawn {
                    return Task::none();
                }
                popup.drawn = true;
                if let Some(session) = &popup.session {
                    tracing::debug!(
                        session = session.id.0,
                        ms = format!("{:.1}", elapsed_ms(session.triggered_at)),
                        "hot path: popup window first redraw"
                    );
                }
                // The window maps with this frame: focus, if granted, follows shortly.
                let wait = Task::perform(sleep(FOCUS_WAIT), move |()| Message::FocusWaitOver(id));
                let refocus = self.host.refocus().map(Message::Popup);
                Task::batch([wait, refocus, self.activate()])
            }
            window::Event::Focused => {
                popup.focused = true;
                popup.had_focus = true;
                let Some(session) = &popup.session else {
                    return self.drop_window();
                };
                tracing::debug!(
                    session = session.id.0,
                    ms = format!("{:.1}", elapsed_ms(session.triggered_at)),
                    "hot path: popup window focused"
                );
                self.start_capture()
            }
            window::Event::Unfocused => {
                popup.focused = false;
                if popup.had_focus && !lingering {
                    self.request_close()
                } else {
                    Task::none()
                }
            }
            window::Event::CloseRequested => self.request_close(),
            window::Event::Resized(size) => {
                tracing::trace!(
                    width = size.width,
                    height = size.height,
                    "popup window resized"
                );
                popup.size = size;
                Task::none()
            }
            window::Event::Closed => {
                // Closed by someone else: end the session without closing it again.
                drop(self.end_session());
                self.popup = None;
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn display(&mut self, display: Option<DisplayPtr>) -> Task<Message> {
        if self.display_known {
            return Task::none();
        }
        self.display_known = true;
        let activator = match display {
            Some(ptr) => {
                let addr = ptr.addr();
                tracing::debug!(addr, "window shell: clipboard on the window's display");
                self.clipboard = Some(FocusClipboard::from_display(ptr));
                self.activation = Activation::Binding;
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || Activator::bind(ptr).map(Arc::new))
                            .await
                            .ok()
                            .flatten()
                    },
                    Message::Activator,
                )
            }
            None => {
                tracing::warn!("window shell: not a Wayland window, no focus-time capture");
                self.activation = Activation::Unavailable;
                Task::none()
            }
        };
        Task::batch([activator, self.start_capture(), self.activate()])
    }

    /// Activates the popup with the trigger's token once it is drawn and xdg-activation is bound.
    fn activate(&mut self) -> Task<Message> {
        let activator = match &self.activation {
            Activation::Ready(activator) => Some(Arc::clone(activator)),
            Activation::Unavailable => None,
            Activation::Unknown | Activation::Binding => return Task::none(),
        };
        let Some(popup) = self.popup.as_mut().filter(|p| p.drawn) else {
            return Task::none();
        };
        let focused = popup.focused;
        let id = popup.id;
        let Some(session) = popup.session.as_mut() else {
            return Task::none();
        };
        let (Some(token), Some(activator)) = (session.token.take(), activator) else {
            return Task::none();
        };
        if focused {
            return Task::none();
        }
        let (session, triggered_at) = (session.id, session.triggered_at);
        window::run(id, move |window| {
            let sent = activator.activate(&token, window);
            tracing::debug!(
                session = session.0,
                sent,
                ms = format!("{:.1}", elapsed_ms(triggered_at)),
                "window shell: xdg-activation requested"
            );
        })
        .discard()
    }

    /// Starts the focus-time capture once focus arrived (or the wait is over) and the clipboard
    /// exists.
    fn start_capture(&mut self) -> Task<Message> {
        let display_known = self.display_known;
        let Some(popup) = self.popup.as_mut() else {
            return Task::none();
        };
        let ready = popup.opened && display_known && (popup.focused || popup.focus_wait_over);
        let focused = popup.focused;
        let Some(session) = popup.session.as_mut() else {
            return Task::none();
        };
        if session.capture != CaptureMode::Focus || session.capturing || !ready {
            return Task::none();
        }
        session.capturing = true;
        let id = session.id;
        let Some(clipboard) = self.clipboard.clone() else {
            return Task::done(Message::Captured(id, Captured::empty()));
        };
        let budget = capture_budget(focused);
        Task::perform(
            async move {
                let primary = clipboard.primary_source().with_budget(budget);
                let fallback = clipboard.clipboard_source().with_budget(budget);
                selection::capture(&[&fallback, &primary], STEP_TIMEOUT).await
            },
            move |captured| Message::Captured(id, captured),
        )
    }

    fn request_close(&mut self) -> Task<Message> {
        let open = self
            .popup
            .as_ref()
            .and_then(|p| p.session.as_ref())
            .is_some_and(|s| !s.closing);
        if !open {
            return Task::none();
        }
        let (task, actions) = self.host.request_close();
        Task::batch([task.map(Message::Popup), self.act(actions)])
    }

    fn act(&mut self, actions: Vec<HostAction>) -> Task<Message> {
        let mut copied = false;
        let mut close = false;
        for action in actions {
            match action {
                HostAction::Copy(text) => {
                    if let Some(clipboard) = &self.clipboard {
                        clipboard.store(text);
                        copied = true;
                    }
                }
                HostAction::Close => close = true,
            }
        }
        if !close {
            return Task::none();
        }
        let Some(popup) = self.popup.as_mut() else {
            return Task::none();
        };
        let id = popup.id;
        let Some(session) = popup.session.as_mut() else {
            return Task::none();
        };
        if session.closing {
            return Task::none();
        }
        if !copied {
            return self.end_session();
        }
        // The store must reach the compositor before focus leaves with the window.
        session.closing = true;
        Task::perform(sleep(COPY_SETTLE), move |()| Message::CloseWindow(id))
    }

    /// Ends the open session and removes the window, or keeps it until its focus settles.
    fn end_session(&mut self) -> Task<Message> {
        let Some(popup) = self.popup.as_mut() else {
            return Task::none();
        };
        if popup.session.take().is_none() {
            return Task::none();
        }
        if let Some(size) = persisted_size(popup.requested, popup.size) {
            self.host.shell().send(ShellEvent::PersistSize(size));
        }
        let (id, settled) = (popup.id, popup.settled());
        self.host.closed();
        if settled {
            return self.drop_window();
        }
        tracing::debug!("window shell: keeping the window until focus settles");
        Task::perform(sleep(LINGER_MAX), move |()| Message::CloseWindow(id))
    }

    fn drop_window(&mut self) -> Task<Message> {
        match self.popup.take() {
            Some(popup) => window::close(popup.id),
            None => Task::none(),
        }
    }

    fn popup_mut(&mut self, id: window::Id) -> Option<&mut Popup> {
        self.popup.as_mut().filter(|p| p.id == id)
    }
}

/// A timer that touches the runtime only when polled.
async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

/// The window size for a persisted popup size: at least [`MIN_SIZE`]. The compositor keeps it on
/// screen.
fn initial_size(persisted: Size) -> Size {
    fit(persisted, Size::new(f32::MAX, f32::MAX))
}

/// The size to persist when the popup closes: only a real change against the request.
fn persisted_size(requested: Size, actual: Size) -> Option<Size> {
    let changed = (requested.width - actual.width).abs() >= SIZE_EPSILON
        || (requested.height - actual.height).abs() >= SIZE_EPSILON;
    (changed && actual.width > 0.0 && actual.height > 0.0).then_some(actual)
}

/// With focus, closed gates are retried for the whole step; without it, one attempt fails fast.
fn capture_budget(focused: bool) -> Duration {
    if focused {
        STEP_TIMEOUT
    } else {
        Duration::ZERO
    }
}
