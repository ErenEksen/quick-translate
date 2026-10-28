//! Throwaway spike for tasks.md 2.1 (D3 overlay host) and 2.2 (iced_layershell
//! capabilities). Not production code.
//!
//! What it does (daemon mode, no initial surface):
//! 1. Enumerates outputs on a side connection (wl_output v4 `name`).
//! 2. "Show": creates one transparent full-output layer surface per output via
//!    `NewLayerShell` with `OutputOption::OutputName` (layer overlay, anchored on
//!    all edges, exclusive zone -1, keyboard exclusive).
//! 3. The first surface that reports a pointer position (a full-surface probe
//!    widget reads `cursor.position()` on `CursorEntered`/`CursorMoved`) wins;
//!    the others are removed with `RemoveWindow`. The panel is drawn at
//!    pointer + 12 px, clamped inside the surface. If nothing arrives within
//!    50 ms the panel is centered on the first surface.
//! 4. Optional: `--input-region` limits the winner's input region to the panel
//!    via `SetInputRegion`; `--cycles N` hides/re-shows N times (dynamic
//!    create/destroy); `--keepalive` keeps a 1x1 transparent surface alive so
//!    the renderer/clipboard survive hide.
//!
//! Everything is logged as `[ms since process start] ...` and
//! `+ms since this surface was requested`.
//!
//! SAFETY NET: hard process exit after `--ms` (default 2000; max 3000 on a
//! live session, max 15000 when WAYLAND_DISPLAY is a nested test socket `qt-*`).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{self, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::widget::{column, container, stack, text};
use iced::{Color, Element, Event, Length, Padding, Point, Rectangle, Size, Subscription, Task, mouse, window};
use iced_layershell::actions::ActionCallback;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::to_layer_message;

const PANEL: Size = Size::new(720.0, 320.0);
const OFFSET: f32 = 12.0;

static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
fn ms() -> f64 {
    T0.get().map(|t| t.elapsed().as_secs_f64() * 1000.0).unwrap_or(0.0)
}
fn log(msg: impl AsRef<str>) {
    println!("[{:8.2} ms] {}", ms(), msg.as_ref());
    let _ = std::io::stdout().flush();
}

#[derive(Debug, Clone)]
struct Opts {
    cycles: u32,
    input_region: bool,
    keepalive: bool,
    kb_late: bool,
    sway_rebase: bool,
    copy_test: bool,
    life_ms: u64,
}

#[derive(Debug, Default)]
struct Win {
    output: String,
    requested: Option<Instant>,
    cycle: u32,
    opened: bool,
    redrawn: bool,
    focused: bool,
    size: Option<Size>,
}

impl Win {
    fn since(&self) -> String {
        self.requested
            .map(|t| format!("+{:.2} ms", t.elapsed().as_secs_f64() * 1000.0))
            .unwrap_or_default()
    }
}

struct App {
    opts: Opts,
    outputs: Vec<String>,
    wins: BTreeMap<window::Id, Win>,
    keepalive: Option<window::Id>,
    winner: Option<window::Id>,
    panel_pos: Option<Point>,
    cycle: u32,
    views: RefCell<BTreeMap<window::Id, bool>>,
    first_key: bool,
    fallback_armed: bool,
}

#[derive(Debug, Clone, Copy)]
enum ProbeKind {
    Entered,
    Moved,
    Pressed,
}

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
enum Message {
    Show,
    Hide,
    Fallback(u32),
    Probe(window::Id, ProbeKind, Point),
    Win(window::Id, window::Event),
    CursorEnteredNoPos(window::Id),
    CursorLeft(window::Id),
    Redraw(window::Id),
    Key(window::Id, iced::keyboard::Key),
    Exit,
}

fn list_outputs() -> Vec<String> {
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{wl_output, wl_registry};
    use wayland_client::{Connection, Dispatch, QueueHandle};
    struct S(Vec<String>);
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for S {
        fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }
    impl Dispatch<wl_output::WlOutput, usize> for S {
        fn event(s: &mut Self, _: &wl_output::WlOutput, e: wl_output::Event, i: &usize, _: &Connection, _: &QueueHandle<Self>) {
            if let wl_output::Event::Name { name } = e {
                s.0[*i] = name;
            }
        }
    }
    let conn = Connection::connect_to_env().expect("wayland");
    let (globals, mut q) = registry_queue_init::<S>(&conn).expect("registry");
    let qh = q.handle();
    let outs: Vec<(u32, u32)> = globals.contents().with_list(|l| {
        l.iter().filter(|g| g.interface == "wl_output").map(|g| (g.name, g.version)).collect()
    });
    let mut s = S(vec![String::new(); outs.len()]);
    let _bound: Vec<wl_output::WlOutput> = outs
        .iter()
        .enumerate()
        .map(|(i, (n, v))| globals.registry().bind(*n, (*v).min(4), &qh, i))
        .collect();
    q.roundtrip(&mut s).expect("roundtrip");
    s.0
}

fn overlay_settings(output: &str, kb: KeyboardInteractivity) -> NewLayerShellSettings {
    NewLayerShellSettings {
        size: None,
        layer: Layer::Overlay,
        anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
        exclusive_zone: Some(-1),
        margin: None,
        keyboard_interactivity: kb,
        output_option: OutputOption::OutputName(output.to_string()),
        events_transparent: false,
        namespace: Some("qt-overlay-probe".into()),
    }
}

fn boot(opts: Opts) -> (App, Task<Message>) {
    let outputs = list_outputs();
    log(format!("outputs: {outputs:?}  opts: {opts:?}"));
    let mut app = App {
        opts: opts.clone(),
        outputs,
        wins: BTreeMap::new(),
        keepalive: None,
        winner: None,
        panel_pos: None,
        cycle: 0,
        views: RefCell::new(BTreeMap::new()),
        first_key: true,
        fallback_armed: false,
    };
    let exit_after = opts.life_ms.saturating_sub(300);
    let mut tasks = vec![];
    if opts.keepalive {
        let (id, t) = Message::layershell_open(NewLayerShellSettings {
            size: Some((1, 1)),
            layer: Layer::Background,
            anchor: Anchor::Top | Anchor::Left,
            exclusive_zone: Some(0),
            margin: None,
            keyboard_interactivity: KeyboardInteractivity::None,
            output_option: OutputOption::Active,
            events_transparent: true,
            namespace: Some("qt-keepalive".into()),
        });
        app.keepalive = Some(id);
        app.wins.insert(id, Win { output: "keepalive".into(), requested: Some(Instant::now()), ..Default::default() });
        log(format!("requested keepalive 1x1 surface {id:?}"));
        tasks.push(t);
        // give the renderer time to initialise before the first Show
        tasks.push(Task::perform(tokio::time::sleep(Duration::from_millis(300)), |_| Message::Show));
    } else {
        tasks.push(Task::done(Message::Show));
    }
    tasks.push(Task::perform(tokio::time::sleep(Duration::from_millis(exit_after)), |_| Message::Exit));
    (app, Task::batch(tasks))
}

fn clamp_panel(pointer: Point, surface: Size) -> Point {
    let x = (pointer.x + OFFSET).min(surface.width - PANEL.width).max(0.0);
    let y = (pointer.y + OFFSET).min(surface.height - PANEL.height).max(0.0);
    Point::new(x, y)
}

fn update(app: &mut App, msg: Message) -> Task<Message> {
    match msg {
        Message::Show => {
            app.cycle += 1;
            app.winner = None;
            app.panel_pos = None;
            app.fallback_armed = false;
            log(format!("=== cycle {} show: creating {} overlay(s)", app.cycle, app.outputs.len()));
            let kb = if app.opts.kb_late { KeyboardInteractivity::None } else { KeyboardInteractivity::Exclusive };
            let mut tasks = vec![];
            for out in app.outputs.clone() {
                let (id, t) = Message::layershell_open(overlay_settings(&out, kb));
                app.wins.insert(id, Win { output: out.clone(), requested: Some(Instant::now()), cycle: app.cycle, ..Default::default() });
                log(format!("requested overlay {id:?} on {out}"));
                tasks.push(t);
            }
            Task::batch(tasks)
        }
        Message::Fallback(c) => {
            if c == app.cycle && app.winner.is_none() {
                let first = app.wins.iter().find(|(id, w)| w.cycle == c && Some(**id) != app.keepalive).map(|(id, _)| *id);
                log(format!("50 ms fallback fired in cycle {c}: no pointer position yet -> center on {first:?}"));
                if let Some(id) = first {
                    let size = app.wins[&id].size.unwrap_or(Size::new(1920.0, 1080.0));
                    app.panel_pos = Some(Point::new((size.width - PANEL.width) / 2.0, (size.height - PANEL.height) / 2.0));
                    return pick_winner(app, id);
                }
            }
            Task::none()
        }
        Message::Probe(id, kind, pos) => {
            let Some(w) = app.wins.get(&id) else { return Task::none() };
            if app.winner.is_none() && w.cycle == app.cycle {
                log(format!("FIRST pointer position via {kind:?} on {id:?} ({}) at ({:.1}, {:.1}) {}", w.output, pos.x, pos.y, w.since()));
                let size = w.size.unwrap_or(Size::new(1920.0, 1080.0));
                app.panel_pos = Some(clamp_panel(pos, size));
                log(format!("panel at {:?} (surface {:?})", app.panel_pos, size));
                return pick_winner(app, id);
            }
            if let ProbeKind::Pressed = kind {
                let inside = app
                    .panel_pos
                    .map(|p| Rectangle::new(p, PANEL).contains(pos))
                    .unwrap_or(false);
                log(format!("button press on {id:?} at ({:.1}, {:.1}) inside_panel={inside}", pos.x, pos.y));
                if !inside {
                    log("outside click -> hide");
                    return Task::done(Message::Hide);
                }
            }
            Task::none()
        }
        Message::Win(id, ev) => {
            let Some(w) = app.wins.get_mut(&id) else { return Task::none() };
            match ev {
                window::Event::Opened { size, .. } => {
                    w.opened = true;
                    w.size = Some(size);
                    log(format!("Opened {id:?} ({}) size {size:?} {}", w.output, w.since()));
                    // The 50 ms placement fallback starts when the surface is configured
                    // and the renderer is attached, not when it is requested.
                    if Some(id) != app.keepalive && !app.fallback_armed {
                        app.fallback_armed = true;
                        if app.opts.sway_rebase {
                            // Sway only sends wl_pointer.enter on motion; a zero relative
                            // move through IPC makes it rebase the cursor and send enter.
                            let r = std::process::Command::new("swaymsg")
                                .args(["seat", "-", "cursor", "move", "0", "0"])
                                .stdout(std::process::Stdio::null())
                                .spawn();
                            log(format!("spawned `swaymsg seat - cursor move 0 0`: ok={}", r.is_ok()));
                        }
                        let c = app.cycle;
                        return Task::perform(tokio::time::sleep(Duration::from_millis(50)), move |_| Message::Fallback(c));
                    }
                }
                window::Event::Resized(size) => {
                    w.size = Some(size);
                    log(format!("Resized {id:?} {size:?} {}", w.since()));
                }
                window::Event::Focused => {
                    let first = !w.focused;
                    w.focused = true;
                    log(format!("Focused {id:?} ({}) first={first} {}", w.output, w.since()));
                }
                window::Event::Unfocused => log(format!("Unfocused {id:?} ({}) {}", w.output, w.since())),
                window::Event::Closed => log(format!("Closed {id:?} ({}) {}", w.output, w.since())),
                _ => {}
            }
            Task::none()
        }
        Message::CursorEnteredNoPos(id) => {
            if let Some(w) = app.wins.get(&id) {
                log(format!("subscription: mouse::Event::CursorEntered on {id:?} ({}) (no position in event) {}", w.output, w.since()));
            }
            Task::none()
        }
        Message::CursorLeft(id) => {
            log(format!("subscription: CursorLeft on {id:?}"));
            Task::none()
        }
        Message::Redraw(id) => {
            if let Some(w) = app.wins.get_mut(&id)
                && !w.redrawn
            {
                w.redrawn = true;
                log(format!("first RedrawRequested {id:?} ({}) {}", w.output, w.since()));
            }
            Task::none()
        }
        Message::Key(id, key) => {
            if app.first_key {
                app.first_key = false;
                log(format!("first key {key:?} delivered to {id:?}"));
            }
            if key == iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape) {
                return Task::done(Message::Exit);
            }
            Task::none()
        }
        Message::Hide => {
            let ids: Vec<_> = app.wins.iter().filter(|(id, w)| Some(**id) != app.keepalive && w.cycle == app.cycle).map(|(id, _)| *id).collect();
            log(format!("cycle {} hide: removing {} surface(s)", app.cycle, ids.len()));
            let mut tasks: Vec<_> = ids.into_iter().map(|id| Task::done(Message::RemoveWindow(id))).collect();
            if app.cycle < app.opts.cycles {
                tasks.push(Task::perform(tokio::time::sleep(Duration::from_millis(150)), |_| Message::Show));
            } else {
                tasks.push(Task::perform(tokio::time::sleep(Duration::from_millis(150)), |_| Message::Exit));
            }
            Task::batch(tasks)
        }
        Message::Exit => {
            log("exit");
            iced::exit()
        }
        _ => Task::none(),
    }
}

fn pick_winner(app: &mut App, id: window::Id) -> Task<Message> {
    app.winner = Some(id);
    let mut tasks = vec![];
    for (other, w) in &app.wins {
        if *other != id && Some(*other) != app.keepalive && w.cycle == app.cycle {
            log(format!("RemoveWindow loser {other:?} ({})", w.output));
            tasks.push(Task::done(Message::RemoveWindow(*other)));
        }
    }
    if app.opts.kb_late {
        log(format!("KeyboardInteractivityChange exclusive on winner {id:?}"));
        tasks.push(Task::done(Message::KeyboardInteractivityChange { id, keyboard_interactivity: KeyboardInteractivity::Exclusive }));
    }
    if app.opts.input_region
        && let Some(p) = app.panel_pos
    {
        let (x, y, w, h) = (p.x as i32, p.y as i32, PANEL.width as i32, PANEL.height as i32);
        log(format!("SetInputRegion on {id:?} -> panel rect ({x},{y},{w},{h})"));
        tasks.push(Task::done(Message::SetInputRegion {
            id,
            callback: ActionCallback::new(move |region| region.add(x, y, w, h)),
        }));
    }
    if app.opts.copy_test {
        log("copy-test: clipboard::write(\"qt-probe-clipboard-OK\"), overlay removed in 200 ms, daemon stays alive");
        tasks.push(iced::clipboard::write("qt-probe-clipboard-OK".to_string()));
        tasks.push(Task::perform(tokio::time::sleep(Duration::from_millis(200)), move |_| Message::RemoveWindow(id)));
    }
    // cycle mode: hide again after a short while
    if app.opts.cycles > 1 {
        tasks.push(Task::perform(tokio::time::sleep(Duration::from_millis(200)), |_| Message::Hide));
    }
    Task::batch(tasks)
}

fn view(app: &App, id: window::Id) -> Element<'_, Message> {
    {
        let mut v = app.views.borrow_mut();
        if !v.contains_key(&id) {
            v.insert(id, true);
            if let Some(w) = app.wins.get(&id) {
                log(format!("first view() {id:?} ({}) {}", w.output, w.since()));
            }
        }
    }
    if Some(id) == app.keepalive {
        return container(text("")).into();
    }
    let probe: Element<'_, Message> = Element::new(Probe { id });
    match (app.winner == Some(id), app.panel_pos) {
        (true, Some(p)) => {
            let w = &app.wins[&id];
            let panel = container(
                column![
                    text("quick-translate overlay probe").size(20),
                    text(format!("output {} | panel at ({:.0}, {:.0}) | cycle {}", w.output, p.x, p.y, app.cycle)),
                    text("Esc exits. Click outside the panel hides."),
                ]
                .spacing(8),
            )
            .padding(16)
            .width(PANEL.width)
            .height(PANEL.height)
            .style(|theme: &iced::Theme| {
                let pal = theme.extended_palette();
                container::Style {
                    background: Some(pal.background.base.color.into()),
                    border: iced::Border { color: pal.primary.strong.color, width: 2.0, radius: 10.0.into() },
                    text_color: Some(pal.background.base.text),
                    ..Default::default()
                }
            });
            let placed = container(panel)
                .padding(Padding { top: p.y, left: p.x, right: 0.0, bottom: 0.0 })
                .width(Length::Fill)
                .height(Length::Fill);
            stack![probe, placed].into()
        }
        _ => probe,
    }
}

fn subscription(_: &App) -> Subscription<Message> {
    Subscription::batch([
        iced::event::listen_with(|e, _status, id| match e {
            Event::Window(ev) => Some(Message::Win(id, ev)),
            Event::Mouse(mouse::Event::CursorEntered) => Some(Message::CursorEnteredNoPos(id)),
            Event::Mouse(mouse::Event::CursorLeft) => Some(Message::CursorLeft(id)),
            Event::Keyboard(iced::keyboard::Event::KeyPressed { key, .. }) => Some(Message::Key(id, key)),
            _ => None,
        }),
        iced::event::listen_raw(|e, _status, id| match e {
            Event::Window(window::Event::RedrawRequested(_)) => Some(Message::Redraw(id)),
            _ => None,
        }),
    ])
}

/// Full-surface, invisible leaf widget that reports the cursor position on
/// enter / motion / press. `CursorEntered` carries no position in iced, but
/// iced_layershell has already stored the `wl_pointer.enter` position in the
/// window state, so `cursor.position()` is available here.
struct Probe {
    id: window::Id,
}

impl<Theme, R: renderer::Renderer> Widget<Message, Theme, R> for Probe {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }
    fn layout(&mut self, _tree: &mut Tree, _r: &R, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.max())
    }
    fn draw(&self, _: &Tree, _: &mut R, _: &Theme, _: &renderer::Style, _: Layout<'_>, _: mouse::Cursor, _: &Rectangle) {}
    fn update(
        &mut self,
        _tree: &mut Tree,
        event: &Event,
        _layout: Layout<'_>,
        cursor: mouse::Cursor,
        _r: &R,
        _c: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _v: &Rectangle,
    ) {
        let kind = match event {
            Event::Mouse(mouse::Event::CursorEntered) => ProbeKind::Entered,
            Event::Mouse(mouse::Event::CursorMoved { .. }) => ProbeKind::Moved,
            Event::Mouse(mouse::Event::ButtonPressed(_)) => ProbeKind::Pressed,
            _ => return,
        };
        // Levitated / unavailable cursors have no position; fall back to raw.
        let pos = cursor.position().or(match cursor {
            mouse::Cursor::Levitating(p) => Some(p),
            _ => None,
        });
        if let Some(p) = pos {
            shell.publish(Message::Probe(self.id, kind, p));
        }
    }
}

impl<'a, Theme: 'a, R: renderer::Renderer + 'a> From<Probe> for Element<'a, Message, Theme, R> {
    fn from(p: Probe) -> Self {
        Element::new(p)
    }
}

#[allow(dead_code)]
fn _unused(_: widget::Id) {}

fn main() -> Result<(), iced_layershell::Error> {
    T0.set(Instant::now()).ok();
    let args: Vec<String> = std::env::args().collect();
    let get = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1).cloned());
    let nested = std::env::var("WAYLAND_DISPLAY").is_ok_and(|d| d.starts_with("qt-"));
    let cap = if nested { 15000 } else { 3000 };
    let opts = Opts {
        cycles: get("--cycles").and_then(|s| s.parse().ok()).unwrap_or(1),
        input_region: args.iter().any(|a| a == "--input-region"),
        keepalive: args.iter().any(|a| a == "--keepalive"),
        kb_late: args.iter().any(|a| a == "--kb-late"),
        sway_rebase: args.iter().any(|a| a == "--sway-rebase"),
        copy_test: args.iter().any(|a| a == "--copy-test"),
        life_ms: get("--ms").and_then(|s| s.parse().ok()).unwrap_or(2000).min(cap),
    };
    let life = opts.life_ms;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(life));
        log(format!("[safety] {life} ms elapsed, hard exit"));
        std::process::exit(0);
    });
    log(format!(
        "env: WAYLAND_DISPLAY={:?} XDG_CURRENT_DESKTOP={:?} WGPU_BACKEND={:?} ICED_BACKEND={:?}",
        std::env::var("WAYLAND_DISPLAY").ok(),
        std::env::var("XDG_CURRENT_DESKTOP").ok(),
        std::env::var("WGPU_BACKEND").ok(),
        std::env::var("ICED_BACKEND").ok()
    ));

    iced_layershell::daemon(move || boot(opts.clone()), || "qt-overlay-probe".to_string(), update, view)
        .style(|_app, theme: &iced::Theme| iced::theme::Style {
            background_color: Color::TRANSPARENT,
            text_color: theme.palette().text,
        })
        .subscription(subscription)
        .settings(Settings {
            layer_settings: LayerShellSettings { start_mode: StartMode::Background, ..Default::default() },
            ..Default::default()
        })
        .run()
}
