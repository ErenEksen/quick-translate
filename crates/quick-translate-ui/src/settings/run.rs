//! The settings process: a normal iced/winit window hosting [`Settings`].

use std::path::PathBuf;
use std::sync::Arc;

use iced::{Element, Task, Theme, window};
use quick_translate_core::{APP_ID, ConfigStore};

use super::{Effect, Message, Runtime, Settings};

/// Where the settings process reads and writes.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub config_path: PathBuf,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            config_path: ConfigStore::default_path(),
        }
    }
}

/// Runs the settings window until it is closed.
pub fn run(options: RunOptions) -> iced::Result {
    let window = window::Settings {
        size: iced::Size::new(660.0, 780.0),
        min_size: Some(iced::Size::new(480.0, 400.0)),
        platform_specific: window::settings::PlatformSpecific {
            application_id: APP_ID.to_owned(),
            ..Default::default()
        },
        ..window::Settings::default()
    };
    iced::application(move || App::boot(options.clone()), App::update, App::view)
        .title("Quick Translate Settings")
        .theme(App::theme)
        .window(window)
        .run()
}

#[derive(Debug, Clone)]
enum AppMessage {
    Booted(Arc<Runtime>, Message),
    Core(Message),
}

struct App {
    core: Settings,
    runtime: Option<Arc<Runtime>>,
}

impl App {
    fn boot(options: RunOptions) -> (Self, Task<AppMessage>) {
        let boot = Task::future(async move {
            let runtime = Arc::new(Runtime::connect(options.config_path).await);
            let loaded = runtime.load().await;
            AppMessage::Booted(runtime, loaded)
        });
        (
            Self {
                core: Settings::new(),
                runtime: None,
            },
            boot,
        )
    }

    fn update(&mut self, message: AppMessage) -> Task<AppMessage> {
        match message {
            AppMessage::Booted(runtime, loaded) => {
                let signals = {
                    let runtime = Arc::clone(&runtime);
                    Task::future(async move { runtime.signals().await })
                        .then(|signals| Task::run(signals, AppMessage::Core))
                };
                self.runtime = Some(runtime);
                Task::batch([self.dispatch(loaded), signals])
            }
            AppMessage::Core(message) => self.dispatch(message),
        }
    }

    fn dispatch(&mut self, message: Message) -> Task<AppMessage> {
        let effects = self.core.update(message);
        Task::batch(effects.into_iter().map(|effect| self.effect(effect)))
    }

    fn effect(&self, effect: Effect) -> Task<AppMessage> {
        match effect {
            Effect::CopyToClipboard(text) => iced::clipboard::write(text),
            Effect::Raise => window::latest().and_then(window::gain_focus),
            effect => match &self.runtime {
                Some(runtime) => {
                    let runtime = Arc::clone(runtime);
                    Task::future(async move { runtime.execute(effect).await })
                        .and_then(|message| Task::done(AppMessage::Core(message)))
                }
                None => Task::none(),
            },
        }
    }

    fn view(&self) -> Element<'_, AppMessage> {
        self.core.view().map(AppMessage::Core)
    }

    fn theme(&self) -> Theme {
        if self.core.dark() {
            Theme::Dark
        } else {
            Theme::Light
        }
    }
}
