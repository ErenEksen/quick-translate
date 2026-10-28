//! Throwaway spike for tasks.md 2.3: iced 0.14 `text_editor` + IME, hosted
//! either on a wlr layer surface (iced_layershell, the D3 popup host) or in a
//! normal winit xdg_toplevel (the D4 `window` shell).
//!
//! usage:
//!   ime-probe --layer  [--ms N] [--manual]
//!   ime-probe --window [--ms N] [--manual]
//!
//! Logs every `input_method::Event` the runtime sees (Opened / Preedit / Commit
//! / Closed) and every text change of the editor, so a tester can verify that
//! composed text (fcitx5-mozc / ibus-anthy Japanese, Turkish via the IME or
//! the plain `tr` XKB layout) is committed exactly once and preedit is shown.
//! Esc exits.
//!
//! SAFETY NET: hard exit after --ms (default 3000). Without --manual the cap is
//! 3000 ms (the layer variant grabs the keyboard exclusively). With --manual the
//! cap is 120000 ms, for a human doing the IME test interactively.

use std::io::Write as _;
use std::time::{Duration, Instant};

use iced::widget::{column, container, text, text_editor};
use iced::{Element, Event, Length, Subscription, Task};

static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
fn log(msg: impl AsRef<str>) {
    let ms = T0.get().map(|t| t.elapsed().as_secs_f64() * 1000.0).unwrap_or(0.0);
    println!("[{ms:9.2} ms] {}", msg.as_ref());
    let _ = std::io::stdout().flush();
}

struct App {
    content: text_editor::Content,
    shell: &'static str,
    last_ime: String,
}

#[iced_layershell::to_layer_message]
#[derive(Debug, Clone)]
enum Message {
    Edit(text_editor::Action),
    Ime(iced::advanced::input_method::Event),
    Key(iced::keyboard::Key),
    Exit,
}

fn new_app(shell: &'static str, exit_after: u64) -> (App, Task<Message>) {
    log(format!("shell={shell}: click/type into the editor; IME events and text changes are logged"));
    let focus = iced::widget::operation::focus(EDITOR_ID);
    (
        App { content: text_editor::Content::new(), shell, last_ime: String::new() },
        Task::batch([
            focus,
            Task::perform(tokio::time::sleep(Duration::from_millis(exit_after)), |_| Message::Exit),
        ]),
    )
}

const EDITOR_ID: &str = "source";

fn update(app: &mut App, msg: Message) -> Task<Message> {
    match msg {
        Message::Edit(action) => {
            let is_edit = action.is_edit();
            app.content.perform(action);
            if is_edit {
                log(format!("text now: {:?}", app.content.text()));
            }
            Task::none()
        }
        Message::Ime(ev) => {
            let s = format!("{ev:?}");
            if s != app.last_ime {
                log(format!("input_method event: {s}"));
                app.last_ime = s;
            }
            Task::none()
        }
        Message::Key(iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)) | Message::Exit => {
            log(format!("exit; final text: {:?}", app.content.text()));
            iced::exit()
        }
        _ => Task::none(),
    }
}

fn view(app: &App) -> Element<'_, Message> {
    container(
        column![
            text(format!("IME probe ({}) - type Japanese/Turkish, Esc exits", app.shell)),
            text_editor(&app.content)
                .id(EDITOR_ID)
                .placeholder("source text...")
                .on_action(Message::Edit)
                .height(Length::Fill),
        ]
        .spacing(8),
    )
    .padding(12)
    .into()
}

fn subscription(_: &App) -> Subscription<Message> {
    iced::event::listen_with(|e, _s, _id| match e {
        Event::InputMethod(ev) => Some(Message::Ime(ev)),
        Event::Keyboard(iced::keyboard::Event::KeyPressed { key, .. }) => Some(Message::Key(key)),
        _ => None,
    })
}

fn main() {
    T0.set(Instant::now()).ok();
    let args: Vec<String> = std::env::args().collect();
    let get = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1).cloned());
    let manual = args.iter().any(|a| a == "--manual");
    let cap = if manual { 120_000 } else { 3000 };
    let life: u64 = get("--ms").and_then(|s| s.parse().ok()).unwrap_or(3000).min(cap);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(life + 300));
        log("[safety] hard exit");
        std::process::exit(0);
    });
    log(format!(
        "env: WAYLAND_DISPLAY={:?} XMODIFIERS={:?} GTK_IM_MODULE={:?} QT_IM_MODULE={:?}",
        std::env::var("WAYLAND_DISPLAY").ok(),
        std::env::var("XMODIFIERS").ok(),
        std::env::var("GTK_IM_MODULE").ok(),
        std::env::var("QT_IM_MODULE").ok()
    ));

    if args.iter().any(|a| a == "--window") {
        let r = iced::application(move || new_app("winit window", life), update, view)
            .title("ime-probe")
            .subscription(subscription)
            .window_size((800.0, 300.0))
            .run();
        log(format!("run result: {r:?}"));
    } else {
        use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
        use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
        let r = iced_layershell::application(move || new_app("layer surface", life), || "qt-ime-probe".to_string(), update, view)
            .subscription(subscription)
            .settings(Settings {
                layer_settings: LayerShellSettings {
                    size: Some((800, 300)),
                    anchor: Anchor::Top,
                    margin: (200, 0, 0, 0),
                    layer: Layer::Overlay,
                    exclusive_zone: 0,
                    keyboard_interactivity: KeyboardInteractivity::Exclusive,
                    start_mode: StartMode::Active,
                    events_transparent: false,
                },
                ..Default::default()
            })
            .run();
        log(format!("run result: {r:?}"));
    }
}
