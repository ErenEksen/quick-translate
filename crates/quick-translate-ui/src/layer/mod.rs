//! Layer shell for compositors with wlr-layer-shell (design D3, spikes 2.1/2.2).
//!
//! On every Show the shell creates one transparent overlay per output (layer `overlay`,
//! anchored on all edges, exclusive zone -1, keyboard exclusive). The first overlay that learns
//! the pointer position gets the panel (see [`placement`]); a click on the transparent area
//! closes the popup. A permanent 1x1 keep-alive surface keeps the renderer and the clipboard
//! alive between popups: without it each popup pays the renderer setup again and copied text
//! disappears when the popup closes.

pub mod geometry;
pub mod overlay;
pub mod placement;

use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::space;
use iced::{
    Color, Element, Event, Point, Rectangle, Size, Subscription, Task, Theme, event, window,
};
use iced_layershell::reexport::{
    Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption,
};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;
use quick_translate_platform::outputs::OutputTracker;
use quick_translate_platform::selection::{self, STEP_TIMEOUT};

use crate::clipboard::FocusClipboard;
use crate::executor::SharedExecutor;
use crate::host::{HostAction, PopupHost};
use crate::popup;
use crate::shell::{CaptureMode, SessionId, ShellCommand, ShellEnd, ShellEvent, ShowRequest};
use overlay::{Overlay, PointerKind};
use placement::{Fallback, LATE_ENTER, Placed, Placement};

/// Layer-shell namespace of the overlays.
pub const NAMESPACE: &str = "quick-translate";
/// Time the panel stays mapped (but hidden) after a copy, so the clipboard owner commits the
/// selection while the overlay still has keyboard focus.
const COPY_SETTLE: Duration = Duration::from_millis(40);

/// A per-compositor hook run once per popup when every overlay is mapped and the pointer
/// position is still unknown. Compositors that only send `wl_pointer.enter` on motion can be
/// asked here to resend it. It must not block. Without a hook the centered fallback applies.
pub trait PointerNudge: Send + Sync {
    fn nudge(&self);
}

/// Everything the layer shell needs from the daemon.
#[derive(Clone)]
pub struct LayerShell {
    pub shell: ShellEnd,
    pub theme: Theme,
    pub outputs: OutputTracker,
    /// The connection the toolkit uses. `None` lets the toolkit connect itself.
    pub connection: Option<wayland_client::Connection>,
    pub nudge: Option<Arc<dyn PointerNudge>>,
    /// Read the selection through the toolkit after keyboard focus (no data-control). Needs
    /// `connection`.
    pub focus_capture: bool,
}

/// Runs the layer shell on the calling thread until [`ShellCommand::Quit`].
pub fn run(settings: LayerShell) -> Result<(), iced_layershell::Error> {
    let connection = settings.connection.clone();
    iced_layershell::daemon(
        move || App::boot(settings.clone()),
        NAMESPACE,
        App::update,
        App::view,
    )
    .theme(|app: &App, _| app.host.theme())
    .style(|_, theme: &Theme| iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme.palette().text,
    })
    .subscription(App::subscription)
    .executor::<SharedExecutor>()
    .settings(Settings {
        layer_settings: LayerShellSettings {
            start_mode: StartMode::Background,
            ..Default::default()
        },
        with_connection: connection.map(Into::into),
        ..Default::default()
    })
    .run()
}

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
pub enum Message {
    Command(ShellCommand),
    Popup(popup::Message),
    Window(window::Id, window::Event),
    FirstFrame(window::Id),
    Input,
    Pointer(window::Id, PointerKind, Point),
    OutsidePress,
    Resize(Rectangle),
    ResizeEnd(Rectangle),
    Fallback(SessionId),
    LateEnterExpired(SessionId),
    Teardown(SessionId),
    FocusCaptured(SessionId, String, bool),
}

struct Session {
    id: SessionId,
    placement: Placement<window::Id>,
    capture: CaptureMode,
    size: Size,
    closing: bool,
    awaiting_frame: bool,
    shown: bool,
    nudged: bool,
}

struct App {
    host: PopupHost,
    outputs: OutputTracker,
    nudge: Option<Arc<dyn PointerNudge>>,
    keepalive: Option<window::Id>,
    session: Option<Session>,
    /// Toolkit selection reader for [`CaptureMode::Focus`].
    focus: Option<FocusClipboard>,
}

fn keepalive_settings() -> NewLayerShellSettings {
    NewLayerShellSettings {
        size: Some((1, 1)),
        layer: Layer::Background,
        anchor: Anchor::Top | Anchor::Left,
        exclusive_zone: Some(0),
        margin: None,
        keyboard_interactivity: KeyboardInteractivity::None,
        output_option: OutputOption::Active,
        events_transparent: true,
        namespace: Some(format!("{NAMESPACE}-keepalive")),
    }
}

fn overlay_settings(output: Option<&str>) -> NewLayerShellSettings {
    NewLayerShellSettings {
        size: None,
        layer: Layer::Overlay,
        anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
        exclusive_zone: Some(-1),
        margin: None,
        keyboard_interactivity: KeyboardInteractivity::Exclusive,
        output_option: output.map_or(OutputOption::Active, |name| {
            OutputOption::OutputName(name.to_owned())
        }),
        events_transparent: false,
        namespace: Some(NAMESPACE.to_owned()),
    }
}

/// Reads CLIPBOARD, then PRIMARY, once the overlay has keyboard focus. Without a reader the
/// popup opens with an empty, editable source pane.
fn focus_capture(focus: Option<&FocusClipboard>, session: SessionId) -> Task<Message> {
    let Some(focus) = focus else {
        return Task::done(Message::FocusCaptured(session, String::new(), false));
    };
    let primary = focus.primary_source();
    let clipboard = focus.clipboard_source();
    Task::perform(
        async move { selection::capture(&[&clipboard, &primary], STEP_TIMEOUT).await },
        move |captured| Message::FocusCaptured(session, captured.text, captured.truncated),
    )
}

fn after(delay: Duration, message: Message) -> Task<Message> {
    Task::perform(tokio::time::sleep(delay), move |()| message.clone())
}

impl App {
    fn boot(settings: LayerShell) -> (Self, Task<Message>) {
        let (keepalive, task) = Message::layershell_open(keepalive_settings());
        // Loads the font database now instead of on the first popup (about 100 ms).
        drop(iced::widget::text_editor::Content::<iced::Renderer>::with_text("warm-up"));
        tracing::debug!("layer shell started");
        let app = Self {
            host: PopupHost::new(settings.shell, settings.theme),
            outputs: settings.outputs,
            nudge: settings.nudge,
            focus: match (&settings.connection, settings.focus_capture) {
                (Some(connection), true) => FocusClipboard::from_connection(connection),
                _ => None,
            },
            keepalive: Some(keepalive),
            session: None,
        };
        (app, task)
    }

    fn shell(&self) -> &ShellEnd {
        self.host.shell()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Command(command) => self.command(command),
            Message::Popup(message) => {
                let (task, actions) = self.host.update(message);
                Task::batch([task.map(Message::Popup), self.actions(actions)])
            }
            Message::Window(id, event) => self.window_event(id, event),
            Message::FirstFrame(id) => {
                if let Some(session) = &mut self.session
                    && session.awaiting_frame
                    && session.placement.winner() == Some(id)
                {
                    session.awaiting_frame = false;
                    self.host.log_first_frame("layer panel drawn");
                }
                Task::none()
            }
            Message::Input => {
                if let Some(session) = &mut self.session {
                    session.placement.interacted();
                }
                Task::none()
            }
            Message::Pointer(id, kind, point) => {
                let Some(session) = &mut self.session else {
                    return Task::none();
                };
                if session.closing {
                    return Task::none();
                }
                match session.placement.pointer(id, kind, point, Instant::now()) {
                    Some(placed) => self.placed(placed),
                    None => Task::none(),
                }
            }
            Message::OutsidePress => {
                let (task, actions) = self.host.request_close();
                Task::batch([task.map(Message::Popup), self.actions(actions)])
            }
            Message::Resize(rect) => {
                if let Some(session) = &mut self.session {
                    session.placement.set_rect(rect);
                }
                Task::none()
            }
            Message::ResizeEnd(rect) => {
                tracing::debug!(width = rect.width, height = rect.height, "panel resized");
                if let Some(session) = &mut self.session {
                    session.placement.set_rect(rect);
                    if rect.size() != session.size {
                        session.size = rect.size();
                        self.host.shell().send(ShellEvent::PersistSize(rect.size()));
                    }
                }
                Task::none()
            }
            Message::Fallback(id) => {
                let Some(session) = self.session.as_mut().filter(|s| s.id == id) else {
                    return Task::none();
                };
                match session.placement.fallback() {
                    Fallback::Placed(placed) => {
                        tracing::debug!(session = id.0, "no pointer position yet, centering");
                        self.placed(placed)
                    }
                    Fallback::NeedSurface => {
                        tracing::debug!(
                            session = id.0,
                            "no pointer position yet, centering on the compositor's output"
                        );
                        let (overlay, task) = Message::layershell_open(overlay_settings(None));
                        session.placement.requested(overlay, None);
                        task
                    }
                    Fallback::Nothing => Task::none(),
                }
            }
            Message::LateEnterExpired(id) => {
                let Some(session) = self.session.as_mut().filter(|s| s.id == id) else {
                    return Task::none();
                };
                let remove = session.placement.late_enter_expired();
                Task::batch(
                    remove
                        .into_iter()
                        .map(|id| Task::done(Message::RemoveWindow(id))),
                )
            }
            Message::FocusCaptured(session, text, truncated) => {
                let (task, actions) = self.host.captured(session, text, truncated);
                Task::batch([task.map(Message::Popup), self.actions(actions)])
            }
            Message::Teardown(id) => {
                if self.session.as_ref().is_some_and(|s| s.id == id) {
                    self.teardown()
                } else {
                    Task::none()
                }
            }
            _ => Task::none(),
        }
    }

    fn command(&mut self, command: ShellCommand) -> Task<Message> {
        match command {
            ShellCommand::Show(request) => {
                let close = self.teardown();
                Task::batch([close, self.show(&request)])
            }
            ShellCommand::Hide => {
                if self.session.is_none() {
                    return Task::none();
                }
                let (task, actions) = self.host.request_close();
                Task::batch([task.map(Message::Popup), self.actions(actions)])
            }
            ShellCommand::Captured {
                session,
                text,
                truncated,
            } => {
                let (task, actions) = self.host.captured(session, text, truncated);
                Task::batch([task.map(Message::Popup), self.actions(actions)])
            }
            ShellCommand::SetTheme(theme) => {
                self.host.set_theme(theme);
                Task::none()
            }
            ShellCommand::Quit => {
                let close = self.teardown();
                Task::batch([close, iced::exit()])
            }
        }
    }

    fn show(&mut self, request: &ShowRequest) -> Task<Message> {
        let open = self.host.open(request).map(Message::Popup);
        let mut placement = Placement::new(request.size);
        // One overlay per output catches the pointer. A single (or unnamed) output gets one
        // overlay without an output, which also hosts the centered fallback; with several, that
        // overlay is only created when the fallback fires, so the compositor puts it on its
        // active output.
        let outputs: Vec<Option<String>> = match self.outputs.names() {
            Some(names) if names.len() > 1 => names.into_iter().map(Some).collect(),
            _ => vec![None],
        };
        let mut tasks = vec![open];
        for output in outputs {
            let (id, task) = Message::layershell_open(overlay_settings(output.as_deref()));
            placement.requested(id, output);
            tasks.push(task);
        }
        tracing::debug!(
            session = request.session.0,
            overlays = placement.ids().len(),
            "showing popup"
        );
        self.session = Some(Session {
            id: request.session,
            placement,
            capture: request.capture,
            size: request.size,
            closing: false,
            awaiting_frame: false,
            shown: false,
            nudged: false,
        });
        Task::batch(tasks)
    }

    fn placed(&mut self, placed: Placed<window::Id>) -> Task<Message> {
        let Some(session) = &mut self.session else {
            return Task::none();
        };
        let mut tasks: Vec<Task<Message>> = placed
            .remove
            .iter()
            .map(|id| Task::done(Message::RemoveWindow(*id)))
            .collect();
        for id in &placed.keep {
            tasks.push(Task::done(Message::KeyboardInteractivityChange {
                id: *id,
                keyboard_interactivity: KeyboardInteractivity::None,
            }));
        }
        if !placed.keep.is_empty() {
            tasks.push(after(LATE_ENTER, Message::LateEnterExpired(session.id)));
        }
        tracing::debug!(
            session = session.id.0,
            x = placed.rect.x,
            y = placed.rect.y,
            by_pointer = placed.by_pointer,
            "panel placed"
        );
        session.awaiting_frame = true;
        tasks.push(self.host.refocus().map(Message::Popup));
        if !session.shown {
            session.shown = true;
            self.host.shell().send(ShellEvent::Shown {
                session: session.id,
            });
        }
        Task::batch(tasks)
    }

    fn actions(&mut self, actions: Vec<HostAction>) -> Task<Message> {
        let mut tasks = Vec::new();
        let mut copied = false;
        for action in actions {
            match action {
                HostAction::Copy(text) => {
                    copied = true;
                    tasks.push(iced::clipboard::write(text));
                }
                HostAction::Close => {
                    let Some(session) = &mut self.session else {
                        continue;
                    };
                    if copied {
                        session.closing = true;
                        tasks.push(after(COPY_SETTLE, Message::Teardown(session.id)));
                    } else {
                        tasks.push(self.teardown());
                    }
                }
            }
        }
        Task::batch(tasks)
    }

    /// Removes every overlay of the session and reports the popup closed.
    fn teardown(&mut self) -> Task<Message> {
        let Some(session) = self.session.take() else {
            return Task::none();
        };
        if self.host.is_open() {
            let (task, _) = self.host.request_close();
            drop(task);
        }
        self.host.closed();
        Task::batch(
            session
                .placement
                .ids()
                .into_iter()
                .map(|id| Task::done(Message::RemoveWindow(id))),
        )
    }

    fn window_event(&mut self, id: window::Id, event: window::Event) -> Task<Message> {
        if Some(id) == self.keepalive {
            if event == window::Event::Closed {
                // The keep-alive's output went away: replace it so the renderer survives.
                tracing::debug!("keep-alive surface closed, recreating it");
                let (keepalive, task) = Message::layershell_open(keepalive_settings());
                self.keepalive = Some(keepalive);
                return task;
            }
            return Task::none();
        }
        let Some(session) = &mut self.session else {
            return Task::none();
        };
        if !session.placement.contains(id) {
            return Task::none();
        }
        match event {
            window::Event::Opened { size, .. } => {
                let (arm, placed) = session.placement.opened(id, size);
                let mut tasks = Vec::new();
                if arm {
                    tracing::debug!(
                        session = session.id.0,
                        "first overlay mapped, waiting for the pointer"
                    );
                    tasks.push(after(
                        placement::FALLBACK_DELAY,
                        Message::Fallback(session.id),
                    ));
                    if session.capture == CaptureMode::Focus {
                        tasks.push(focus_capture(self.focus.as_ref(), session.id));
                    }
                }
                if !session.nudged && session.placement.waiting_for_pointer() {
                    session.nudged = true;
                    if let Some(nudge) = &self.nudge {
                        tracing::debug!(
                            session = session.id.0,
                            "overlays mapped, nudging the pointer"
                        );
                        nudge.nudge();
                    }
                }
                if let Some(placed) = placed {
                    tasks.push(self.placed(placed));
                }
                Task::batch(tasks)
            }
            window::Event::Resized(size) => {
                session.placement.resized(id, size);
                Task::none()
            }
            window::Event::Closed => {
                let winner = session.placement.winner() == Some(id);
                session.placement.closed(id);
                if winner && !session.closing {
                    // The winner's output disappeared.
                    let (task, actions) = self.host.request_close();
                    return Task::batch([task.map(Message::Popup), self.actions(actions)]);
                }
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        if Some(id) == self.keepalive {
            // Text keeps the glyph pipeline warm for the first popup.
            return iced::widget::text(" ").size(1).into();
        }
        let Some(session) = &self.session else {
            return space().into();
        };
        if !session.placement.contains(id) {
            return space().into();
        }
        let base = Overlay::new()
            .on_pointer(move |kind, point| Message::Pointer(id, kind, point))
            .on_outside_press(Message::OutsidePress);
        let panel = if session.closing || session.placement.winner() != Some(id) {
            None
        } else {
            session.placement.rect().zip(self.host.view())
        };
        match panel {
            Some((rect, content)) => base
                .panel(content.map(Message::Popup), rect)
                .on_resize(Message::Resize)
                .on_resize_end(Message::ResizeEnd)
                .into(),
            None => base.into(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            self.shell().commands.subscription().map(Message::Command),
            event::listen_with(|event, _, id| match event {
                Event::Window(event) => Some(Message::Window(id, event)),
                _ => None,
            }),
        ];
        if self.session.is_some() {
            subscriptions.push(popup::subscription().map(Message::Popup));
            subscriptions.push(event::listen_raw(|event, _, _| match event {
                Event::Keyboard(iced::keyboard::Event::KeyPressed { .. }) => Some(Message::Input),
                _ => None,
            }));
        }
        if self.session.as_ref().is_some_and(|s| s.awaiting_frame) {
            subscriptions.push(event::listen_raw(|event, _, id| match event {
                Event::Window(window::Event::RedrawRequested(_)) => Some(Message::FirstFrame(id)),
                _ => None,
            }));
        }
        Subscription::batch(subscriptions)
    }
}
