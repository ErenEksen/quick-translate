//! Shows the popup panel in a normal window with a fake streaming backend.
//!
//! `QT_PREVIEW_SECS` (default 5, `0` keeps it open) closes the window automatically;
//! `QT_PREVIEW_DARK=1` uses the dark theme; `QT_PREVIEW_UNCONFIGURED=1` shows the settings action.
//! `QT_PREVIEW_TRACE=1` prints message kinds, view rebuilds and redraw events (never text).

use std::time::Duration;

use futures::stream;
use iced::{Element, Subscription, Task, Theme};
use quick_translate_backends::Chunk;
use quick_translate_ui::popup::{
    self, BackendInfo, Effect, Languages, Message, OpenOptions, Popup, TranslationId,
};

struct Preview {
    popup: Popup,
}

#[derive(Debug, Clone)]
enum Msg {
    Popup(Message),
    Quit,
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

fn boot() -> (Preview, Task<Msg>) {
    let popup = Popup::new(OpenOptions {
        target_lang: "TR".into(),
        languages: Languages::deepl(),
        backend: BackendInfo {
            label: "DeepL".into(),
            configured: !env_flag("QT_PREVIEW_UNCONFIGURED"),
            reports_detected: true,
        },
        splitter: 0.5,
        theme: if env_flag("QT_PREVIEW_DARK") {
            Theme::Dark
        } else {
            Theme::Light
        },
    });

    // Capture "arrives" after 150 ms so the placeholder is visible briefly.
    let capture = Task::future(async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        Msg::Popup(Message::Captured {
            text: "The quick brown fox jumps over the lazy dog.\nSecond line.".into(),
            truncated: false,
        })
    });
    let secs: u64 = std::env::var("QT_PREVIEW_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let quit = if secs == 0 {
        Task::none()
    } else {
        Task::future(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            Msg::Quit
        })
    };
    (Preview { popup }, Task::batch([capture, quit]))
}

/// Streams a canned translation word by word.
fn fake_translation(id: TranslationId) -> Task<Msg> {
    let words: Vec<String> =
        "Hızlı kahverengi tilki tembel köpeğin üzerinden atlar.\nİkinci satır."
            .split_inclusive(' ')
            .map(str::to_owned)
            .collect();
    let mut items = vec![Message::Chunk {
        id,
        chunk: Chunk::Detected("EN".into()),
    }];
    items.extend(words.into_iter().map(|w| Message::Chunk {
        id,
        chunk: Chunk::Delta(w),
    }));
    items.push(Message::TranslationFinished { id });
    let stream = stream::unfold(items.into_iter(), |mut items| async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        items.next().map(|m| (Msg::Popup(m), items))
    });
    Task::stream(stream)
}

fn update(state: &mut Preview, msg: Msg) -> Task<Msg> {
    let message = match msg {
        Msg::Quit => return iced::exit(),
        Msg::Popup(message) => message,
    };
    if std::env::var_os("QT_PREVIEW_TRACE").is_some() {
        eprintln!("preview: {:?}", std::mem::discriminant(&message));
    }
    let tasks = state
        .popup
        .update(message)
        .into_iter()
        .map(|effect| match effect.into_task() {
            Ok(task) => task.map(Msg::Popup),
            Err(Effect::StartTranslation { id, .. }) => fake_translation(id),
            Err(Effect::CopyToClipboard(text)) => iced::clipboard::write(text),
            Err(Effect::Close) => iced::exit(),
            Err(other) => {
                eprintln!(
                    "preview: unhandled effect {:?}",
                    std::mem::discriminant(&other)
                );
                Task::none()
            }
        });
    Task::batch(tasks.collect::<Vec<_>>())
}

fn view(state: &Preview) -> Element<'_, Msg> {
    if std::env::var_os("QT_PREVIEW_TRACE").is_some() {
        eprintln!("preview: view, status {:?}", state.popup.status());
    }
    state.popup.view().map(Msg::Popup)
}

fn subscription(_state: &Preview) -> Subscription<Msg> {
    let trace = if std::env::var_os("QT_PREVIEW_TRACE").is_some() {
        iced::event::listen_raw(|event, _, _| {
            if let iced::Event::Window(iced::window::Event::RedrawRequested(_)) = event {
                eprintln!("preview: redraw");
            }
            None
        })
    } else {
        Subscription::none()
    };
    Subscription::batch([popup::subscription().map(Msg::Popup), trace])
}

fn main() -> iced::Result {
    iced::application(boot, update, view)
        .title("Quick Translate popup preview")
        .subscription(subscription)
        .theme(|state: &Preview| state.popup.theme())
        .window_size((720.0, 320.0))
        .run()
}
