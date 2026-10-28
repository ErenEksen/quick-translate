//! Popup app core: state, messages and update logic shared by all shells (design D4).
//!
//! The core is shell-agnostic. A shell owns a [`Popup`], feeds it [`Message`]s, renders
//! [`Popup::view`] inside its own surface and executes the [`Effect`]s returned by
//! [`Popup::update`]. Effects that only need the iced runtime (debounce timers and widget
//! focus) can be turned into tasks with [`Effect::into_task`]; the rest (translation
//! requests, clipboard, closing, persistence, settings) are the shell's job.

mod keys;
mod split;
mod task;
mod view;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Duration;

use iced::Theme;
use iced::widget::text_editor::{Action, Content, Edit, Motion};
use quick_translate_backends::{Chunk, Error, Request};
use quick_translate_core::language::{self, Language};

pub use keys::{Key, subscription};
pub use split::Split;
pub use task::{focus_task, translation_stream, translation_task};

/// Quiet period after the last source edit before a new translation starts.
pub const DEBOUNCE: Duration = Duration::from_millis(400);
/// Smallest fraction of the panel width the source pane may take.
pub const SPLITTER_MIN: f32 = 0.2;
/// Largest fraction of the panel width the source pane may take.
pub const SPLITTER_MAX: f32 = 0.8;
/// Splitter movement per arrow key press when the splitter has keyboard focus.
pub const SPLITTER_KEY_STEP: f32 = 0.05;

/// Identifies one translation request. Messages carrying a stale id are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TranslationId(pub u64);

/// What the popup needs to know about the active backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    /// Indicator text, e.g. `DeepL` or `AI: gpt-4o-mini`.
    pub label: String,
    /// Credentials or endpoint are present. When false the translation pane offers to open settings.
    pub configured: bool,
    /// The backend emits [`Chunk::Detected`] (DeepL). When false the source shows plain "Auto".
    pub reports_detected: bool,
}

/// Languages offered by the selectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Languages {
    pub sources: &'static [Language],
    pub targets: &'static [Language],
}

impl Languages {
    /// The DeepL language lists from `quick-translate-core`.
    pub fn deepl() -> Self {
        Self {
            sources: language::source_languages(),
            targets: language::target_languages(),
        }
    }
}

impl Default for Languages {
    fn default() -> Self {
        Self::deepl()
    }
}

/// Everything a shell passes when a popup opens.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    /// Remembered or locale-derived target code (`EN-US`, `TR`, ...).
    pub target_lang: String,
    pub languages: Languages,
    pub backend: BackendInfo,
    /// Persisted splitter ratio (fraction of the width given to the source pane).
    pub splitter: f32,
    pub theme: Theme,
}

/// A keyboard-focusable control, in Tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Control {
    SourceLanguage,
    Swap,
    TargetLanguage,
    Copy,
    /// Gear icon in the header.
    HeaderSettings,
    SourcePane,
    Splitter,
    TranslationPane,
    /// "Open settings" action shown when the backend is not configured.
    Settings,
}

impl Control {
    const ORDER: [Self; 9] = [
        Self::SourceLanguage,
        Self::Swap,
        Self::TargetLanguage,
        Self::Copy,
        Self::HeaderSettings,
        Self::SourcePane,
        Self::Splitter,
        Self::TranslationPane,
        Self::Settings,
    ];

    fn index(self) -> usize {
        Self::ORDER.iter().position(|c| *c == self).unwrap_or(0)
    }

    fn pane(self) -> Option<Pane> {
        match self {
            Self::SourcePane => Some(Pane::Source),
            Self::TranslationPane => Some(Pane::Translation),
            _ => None,
        }
    }
}

/// One of the two text panes. These are the only iced-focusable widgets in the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pane {
    Source,
    Translation,
}

impl Pane {
    /// The iced widget id of the pane's text editor.
    pub fn widget_id(self) -> iced::widget::Id {
        match self {
            Self::Source => iced::widget::Id::new("quick-translate-source"),
            Self::Translation => iced::widget::Id::new("quick-translate-translation"),
        }
    }
}

/// State of the translation pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslationStatus {
    /// Nothing requested (empty source, or waiting for capture or debounce).
    Idle,
    /// Request sent, nothing received yet.
    Loading,
    /// Receiving deltas.
    Streaming,
    /// The translation is complete.
    Done,
    Failed(Error),
    /// The active backend has no credentials or endpoint.
    NotConfigured,
}

/// Input to [`Popup::update`].
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Selection capture finished. Empty text yields an empty, focused source pane.
    Captured {
        text: String,
        truncated: bool,
    },
    SourceAction(Action),
    TranslationAction(Action),
    /// A debounce timer scheduled by [`Effect::ScheduleDebounce`] fired.
    DebounceElapsed(u64),
    Chunk {
        id: TranslationId,
        chunk: Chunk,
    },
    TranslationFinished {
        id: TranslationId,
    },
    TranslationFailed {
        id: TranslationId,
        error: Error,
    },
    /// `None` selects auto-detect.
    SourceLanguageSelected(Option<&'static str>),
    TargetLanguageSelected(&'static str),
    /// A language menu was opened with the mouse; keyboard focus follows it.
    SelectorOpened(Control),
    SwapPressed,
    CopyPressed,
    OpenSettingsPressed,
    SplitterDragged(f32),
    SplitterReleased,
    /// A key press not consumed by a widget (see [`subscription`]).
    Key(Key),
    /// The shell wants the popup gone: outside click, focus loss or trigger toggle.
    CloseRequested,
}

/// Side effects requested by [`Popup::update`], executed in order by the shell.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Start a backend request and report back with `Chunk`/`TranslationFinished`/`TranslationFailed`.
    StartTranslation { id: TranslationId, request: Request },
    /// Abort the request with this id (cancel its token).
    CancelTranslation(TranslationId),
    /// Send `Message::DebounceElapsed(generation)` after `after`.
    ScheduleDebounce { generation: u64, after: Duration },
    /// Put this text on CLIPBOARD. The daemon must keep serving it after the popup closes.
    CopyToClipboard(String),
    /// Hide the popup.
    Close,
    /// Launch the settings app.
    OpenSettings,
    /// Remember the target language in the config.
    PersistTarget(String),
    /// Remember the splitter ratio in the config.
    PersistSplitter(f32),
    /// Give iced focus to a pane's text editor, or unfocus both when `None`.
    Focus(Option<Pane>),
}

impl Effect {
    /// Converts effects that only need the iced runtime into a task: debounce timers (tokio
    /// sleep) and widget focus. Every other effect is handed back for the shell to execute.
    pub fn into_task(self) -> Result<iced::Task<Message>, Self> {
        match self {
            Self::ScheduleDebounce { generation, after } => Ok(iced::Task::future(async move {
                tokio::time::sleep(after).await;
                Message::DebounceElapsed(generation)
            })),
            Self::Focus(pane) => Ok(focus_task(pane)),
            other => Err(other),
        }
    }
}

/// The popup panel: two panes, header controls and translation lifecycle.
pub struct Popup {
    languages: Languages,
    backend: BackendInfo,
    theme: Theme,
    /// `None` means auto-detect.
    source_lang: Option<&'static str>,
    target_lang: &'static str,
    persisted_target: &'static str,
    detected: Option<String>,
    source: Content,
    translation: Content,
    translation_text: String,
    capture_pending: bool,
    truncated: bool,
    status: TranslationStatus,
    current: Option<TranslationId>,
    last_request: Option<Request>,
    next_id: u64,
    generation: u64,
    splitter: f32,
    focus: Option<Control>,
    notice: Option<String>,
}

impl Popup {
    /// A freshly opened popup waiting for selection capture.
    pub fn new(options: OpenOptions) -> Self {
        let mut popup = Self {
            languages: options.languages,
            backend: options.backend.clone(),
            theme: options.theme.clone(),
            source_lang: None,
            target_lang: "",
            persisted_target: "",
            detected: None,
            source: Content::new(),
            translation: Content::new(),
            translation_text: String::new(),
            capture_pending: true,
            truncated: false,
            status: TranslationStatus::Idle,
            current: None,
            last_request: None,
            next_id: 0,
            generation: 0,
            splitter: 0.5,
            focus: None,
            notice: None,
        };
        // A fresh popup has nothing in flight, so reset emits no effects.
        let _ = popup.reset(options);
        popup
    }

    /// Reuses this popup for a new activation. Cancels anything still in flight; translation
    /// ids keep increasing so late messages from the previous activation are ignored.
    #[must_use]
    pub fn reset(&mut self, options: OpenOptions) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.cancel_current(&mut fx);
        self.target_lang = resolve_target(&options.languages, &options.target_lang);
        self.persisted_target = self.target_lang;
        self.languages = options.languages;
        self.backend = options.backend;
        self.theme = options.theme;
        self.source_lang = None;
        self.detected = None;
        self.source = Content::new();
        self.clear_translation();
        self.capture_pending = true;
        self.truncated = false;
        self.status = self.idle_status();
        self.last_request = None;
        self.generation += 1;
        self.splitter = clamp_splitter(options.splitter);
        self.focus = None;
        fx
    }

    /// Processes one message and returns the effects the shell must execute, in order.
    #[must_use]
    pub fn update(&mut self, message: Message) -> Vec<Effect> {
        let mut fx = Vec::new();
        match message {
            Message::Captured { text, truncated } => self.captured(&text, truncated, &mut fx),
            Message::SourceAction(action) => self.source_action(action, &mut fx),
            Message::TranslationAction(action) => {
                if matches!(action, Action::Click(_)) {
                    self.focus = Some(Control::TranslationPane);
                }
                if !action.is_edit() {
                    self.translation.perform(action);
                }
            }
            Message::DebounceElapsed(generation) => {
                if generation == self.generation {
                    self.persist_target(&mut fx);
                    self.request_translation(&mut fx);
                }
            }
            Message::Chunk { id, chunk } => self.chunk(id, chunk),
            Message::TranslationFinished { id } => {
                if self.current == Some(id) {
                    self.current = None;
                    self.status = TranslationStatus::Done;
                }
            }
            Message::TranslationFailed { id, error } => {
                if self.current == Some(id) {
                    self.current = None;
                    self.status = match error {
                        Error::NotConfigured => TranslationStatus::NotConfigured,
                        Error::Cancelled => TranslationStatus::Idle,
                        error => TranslationStatus::Failed(error),
                    };
                }
            }
            Message::SourceLanguageSelected(code) => {
                self.focus = Some(Control::SourceLanguage);
                self.set_source(code, &mut fx);
            }
            Message::TargetLanguageSelected(code) => {
                self.focus = Some(Control::TargetLanguage);
                self.set_target(code, &mut fx);
            }
            Message::SelectorOpened(control) => {
                self.focus = Some(control);
                fx.push(Effect::Focus(None));
            }
            Message::SwapPressed => {
                self.focus = Some(Control::Swap);
                self.swap(&mut fx);
            }
            Message::CopyPressed => self.copy_all(&mut fx),
            Message::OpenSettingsPressed => self.open_settings(&mut fx),
            Message::SplitterDragged(ratio) => self.splitter = clamp_splitter(ratio),
            Message::SplitterReleased => fx.push(Effect::PersistSplitter(self.splitter)),
            Message::Key(key) => self.key(key, &mut fx),
            Message::CloseRequested => self.close(&mut fx),
        }
        fx
    }

    /// The theme the shell should render the popup with.
    pub fn theme(&self) -> Theme {
        self.theme.clone()
    }

    /// Switches between light and dark (system color-scheme changes).
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// Shows a one-line warning under the header, e.g. a malformed config file.
    pub fn set_notice(&mut self, notice: Option<String>) {
        self.notice = notice;
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Current source code, `None` for auto-detect.
    pub fn source_lang(&self) -> Option<&'static str> {
        self.source_lang
    }

    /// Current target code.
    pub fn target_lang(&self) -> &'static str {
        self.target_lang
    }

    /// Language detected by the backend for the current text, if reported.
    pub fn detected_lang(&self) -> Option<&str> {
        self.detected.as_deref()
    }

    /// Text in the source pane.
    pub fn source_text(&self) -> String {
        self.source.text()
    }

    /// Translation received so far.
    pub fn translation_text(&self) -> &str {
        &self.translation_text
    }

    pub fn status(&self) -> &TranslationStatus {
        &self.status
    }

    /// The request whose results are currently accepted.
    pub fn current_translation(&self) -> Option<TranslationId> {
        self.current
    }

    /// Selection capture has not reported yet; the source pane shows a placeholder.
    pub fn is_capture_pending(&self) -> bool {
        self.capture_pending
    }

    /// The captured selection was cut to `MAX_SOURCE_CHARS`.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    pub fn splitter(&self) -> f32 {
        self.splitter
    }

    /// The control holding keyboard focus.
    pub fn focused(&self) -> Option<Control> {
        self.focus
    }

    /// The swap button is usable: a finished translation exists and the new target is known.
    pub fn can_swap(&self) -> bool {
        self.status == TranslationStatus::Done
            && !self.translation_text.is_empty()
            && self.swap_target().is_some()
    }

    /// A complete translation exists for the Copy button and Ctrl+C without selection.
    pub fn can_copy_all(&self) -> bool {
        self.status == TranslationStatus::Done && !self.translation_text.is_empty()
    }

    fn idle_status(&self) -> TranslationStatus {
        if self.backend.configured {
            TranslationStatus::Idle
        } else {
            TranslationStatus::NotConfigured
        }
    }

    fn captured(&mut self, text: &str, truncated: bool, fx: &mut Vec<Effect>) {
        self.capture_pending = false;
        self.truncated = truncated;
        self.source = Content::with_text(text);
        self.focus = Some(Control::SourcePane);
        fx.push(Effect::Focus(Some(Pane::Source)));
        self.request_translation(fx);
    }

    fn source_action(&mut self, action: Action, fx: &mut Vec<Effect>) {
        if self.capture_pending {
            return;
        }
        if matches!(action, Action::Click(_)) {
            self.focus = Some(Control::SourcePane);
        }
        let is_edit = action.is_edit();
        self.source.perform(action);
        if is_edit {
            // The in-flight request is for superseded text.
            self.cancel_current(fx);
            self.schedule_debounce(fx);
        }
    }

    fn schedule_debounce(&mut self, fx: &mut Vec<Effect>) {
        self.generation += 1;
        fx.push(Effect::ScheduleDebounce {
            generation: self.generation,
            after: DEBOUNCE,
        });
    }

    fn chunk(&mut self, id: TranslationId, chunk: Chunk) {
        if self.current != Some(id) {
            return;
        }
        match chunk {
            Chunk::Delta(delta) => {
                self.status = TranslationStatus::Streaming;
                self.append_translation(&delta);
            }
            Chunk::Detected(code) => {
                if self.backend.reports_detected {
                    self.detected = Some(code);
                }
            }
        }
    }

    fn append_translation(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.translation_text.push_str(delta);
        self.translation.perform(Action::Move(Motion::DocumentEnd));
        self.translation
            .perform(Action::Edit(Edit::Paste(Arc::new(delta.to_owned()))));
    }

    fn clear_translation(&mut self) {
        self.translation = Content::new();
        self.translation_text.clear();
    }

    fn cancel_current(&mut self, fx: &mut Vec<Effect>) {
        if let Some(id) = self.current.take() {
            fx.push(Effect::CancelTranslation(id));
            if matches!(
                self.status,
                TranslationStatus::Loading | TranslationStatus::Streaming
            ) {
                self.status = TranslationStatus::Idle;
            }
        }
    }

    /// Cancels whatever is running and starts a request for the current text and languages.
    fn request_translation(&mut self, fx: &mut Vec<Effect>) {
        // Supersedes any pending debounce.
        self.generation += 1;
        if self.capture_pending {
            return;
        }
        let request = Request {
            text: self.source.text(),
            source_lang: self.source_lang.map(str::to_owned),
            target_lang: self.target_lang.to_owned(),
        };
        if self.status == TranslationStatus::Done && self.last_request.as_ref() == Some(&request) {
            return;
        }
        self.cancel_current(fx);
        self.clear_translation();
        if self.source_lang.is_none() {
            self.detected = None;
        }
        if !self.backend.configured {
            self.status = TranslationStatus::NotConfigured;
            return;
        }
        if request.text.trim().is_empty() {
            self.status = TranslationStatus::Idle;
            self.last_request = None;
            return;
        }
        self.next_id += 1;
        let id = TranslationId(self.next_id);
        self.current = Some(id);
        self.status = TranslationStatus::Loading;
        self.last_request = Some(request.clone());
        fx.push(Effect::StartTranslation { id, request });
    }

    fn persist_target(&mut self, fx: &mut Vec<Effect>) {
        if self.target_lang != self.persisted_target {
            self.persisted_target = self.target_lang;
            fx.push(Effect::PersistTarget(self.target_lang.to_owned()));
        }
    }

    fn set_source(&mut self, code: Option<&'static str>, fx: &mut Vec<Effect>) {
        let code = code.and_then(|c| find(self.languages.sources, c));
        if code == self.source_lang {
            return;
        }
        self.source_lang = code;
        self.detected = None;
        self.request_translation(fx);
    }

    fn set_target(&mut self, code: &'static str, fx: &mut Vec<Effect>) {
        let Some(code) = find(self.languages.targets, code) else {
            return;
        };
        if code != self.target_lang {
            self.target_lang = code;
            self.request_translation(fx);
        }
        self.persist_target(fx);
    }

    /// Target for a swap: the explicit source, or the detected one when on auto.
    fn swap_target(&self) -> Option<&'static str> {
        let source = match self.source_lang {
            Some(code) => code,
            None => self.detected.as_deref()?,
        };
        language::source_to_target(source).and_then(|c| find(self.languages.targets, c))
    }

    fn swap(&mut self, fx: &mut Vec<Effect>) {
        if !self.can_swap() {
            return;
        }
        let Some(new_target) = self.swap_target() else {
            return;
        };
        let new_source = language::target_to_source(self.target_lang)
            .and_then(|c| find(self.languages.sources, c));
        let text = std::mem::take(&mut self.translation_text);
        self.source = Content::with_text(&text);
        self.source_lang = new_source;
        self.target_lang = new_target;
        self.detected = None;
        self.persist_target(fx);
        self.request_translation(fx);
    }

    /// Selection in the focused pane first, then the translation pane, then the source pane.
    fn selected_text(&self) -> Option<String> {
        let order = match self.focus {
            Some(Control::SourcePane) => [&self.source, &self.translation],
            _ => [&self.translation, &self.source],
        };
        order
            .into_iter()
            .filter_map(Content::selection)
            .find(|s| !s.is_empty())
    }

    fn copy_shortcut(&mut self, fx: &mut Vec<Effect>) {
        let text = self
            .selected_text()
            .or_else(|| self.can_copy_all().then(|| self.translation_text.clone()));
        if let Some(text) = text {
            fx.push(Effect::CopyToClipboard(text));
            self.close(fx);
        }
    }

    fn copy_all(&mut self, fx: &mut Vec<Effect>) {
        if self.can_copy_all() {
            fx.push(Effect::CopyToClipboard(self.translation_text.clone()));
            self.close(fx);
        }
    }

    fn open_settings(&mut self, fx: &mut Vec<Effect>) {
        fx.push(Effect::OpenSettings);
        // The settings window would open beneath the overlay, so the popup goes away.
        self.close(fx);
    }

    fn close(&mut self, fx: &mut Vec<Effect>) {
        self.cancel_current(fx);
        // Invalidate any pending debounce.
        self.generation += 1;
        fx.push(Effect::Close);
    }

    fn is_enabled(&self, control: Control) -> bool {
        let not_configured = self.status == TranslationStatus::NotConfigured;
        match control {
            Control::Swap => self.can_swap(),
            Control::Copy => self.can_copy_all(),
            Control::SourcePane => !self.capture_pending,
            Control::TranslationPane => !not_configured,
            Control::Settings => not_configured,
            Control::SourceLanguage
            | Control::TargetLanguage
            | Control::HeaderSettings
            | Control::Splitter => true,
        }
    }

    fn move_focus(&mut self, forward: bool, fx: &mut Vec<Effect>) {
        let len = Control::ORDER.len();
        let start = match self.focus {
            Some(control) => control.index(),
            None if forward => len - 1,
            None => 0,
        };
        let next = (1..=len)
            .map(|step| {
                let index = if forward {
                    (start + step) % len
                } else {
                    (start + len - step) % len
                };
                Control::ORDER[index]
            })
            .find(|c| self.is_enabled(*c));
        if let Some(control) = next {
            self.focus = Some(control);
            fx.push(Effect::Focus(control.pane()));
        }
    }

    fn key(&mut self, key: Key, fx: &mut Vec<Effect>) {
        match key {
            Key::Copy => return self.copy_shortcut(fx),
            Key::Escape => return self.close(fx),
            Key::Tab => return self.move_focus(true, fx),
            Key::BackTab => return self.move_focus(false, fx),
            _ => {}
        }
        let Some(control) = self.focus else {
            return;
        };
        let activate = matches!(key, Key::Enter | Key::Space);
        match control {
            Control::SourceLanguage | Control::TargetLanguage => {
                if activate {
                    self.persist_target(fx);
                    self.request_translation(fx);
                } else {
                    self.step_language(control, key, fx);
                }
            }
            Control::Swap if activate => self.swap(fx),
            Control::Copy if activate => self.copy_all(fx),
            Control::Settings | Control::HeaderSettings if activate => self.open_settings(fx),
            Control::Splitter => {
                let delta = match key {
                    Key::Left => -SPLITTER_KEY_STEP,
                    Key::Right => SPLITTER_KEY_STEP,
                    _ => return,
                };
                self.splitter = clamp_splitter(self.splitter + delta);
                fx.push(Effect::PersistSplitter(self.splitter));
            }
            _ => {}
        }
    }

    /// Arrow/Home/End/letter keys on a focused language selector. The choice applies at once;
    /// the new request waits for the debounce so browsing with arrows does not spam the backend.
    fn step_language(&mut self, control: Control, key: Key, fx: &mut Vec<Effect>) {
        let changed = if control == Control::TargetLanguage {
            let list = self.languages.targets;
            let index = list.iter().position(|l| l.code == self.target_lang);
            match pick_index(list, index, key) {
                Some(i) if Some(i) != index => {
                    self.target_lang = list[i].code;
                    true
                }
                _ => false,
            }
        } else {
            // Index 0 is auto-detect, languages follow.
            let list = self.languages.sources;
            let index = match self.source_lang {
                None => 0,
                Some(code) => list
                    .iter()
                    .position(|l| l.code == code)
                    .map_or(0, |i| i + 1),
            };
            match pick_source_index(list, index, key) {
                Some(i) if i != index => {
                    self.source_lang = i.checked_sub(1).map(|i| list[i].code);
                    self.detected = None;
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.cancel_current(fx);
            self.schedule_debounce(fx);
        }
    }
}

/// New index into `list` for a navigation key, wrapping-free like a native select.
fn pick_index(list: &[Language], current: Option<usize>, key: Key) -> Option<usize> {
    let last = list.len().checked_sub(1)?;
    match key {
        Key::Up => Some(current.map_or(0, |i| i.saturating_sub(1))),
        Key::Down => Some(current.map_or(0, |i| (i + 1).min(last))),
        Key::Home => Some(0),
        Key::End => Some(last),
        Key::Char(c) => jump_to_letter(list.iter().map(|l| l.name), current, c),
        _ => None,
    }
}

/// Like [`pick_index`] for the source list, where index 0 is auto-detect.
fn pick_source_index(list: &[Language], current: usize, key: Key) -> Option<usize> {
    let last = list.len();
    match key {
        Key::Up => Some(current.saturating_sub(1)),
        Key::Down => Some((current + 1).min(last)),
        Key::Home => Some(0),
        Key::End => Some(last),
        Key::Char(c) => {
            let names = std::iter::once("Auto-detect").chain(list.iter().map(|l| l.name));
            jump_to_letter(names, Some(current), c)
        }
        _ => None,
    }
}

/// Next entry after `current` whose name starts with `letter`, wrapping around.
fn jump_to_letter<'a>(
    names: impl Iterator<Item = &'a str>,
    current: Option<usize>,
    letter: char,
) -> Option<usize> {
    let names: Vec<&str> = names.collect();
    let start = current.map_or(0, |i| i + 1);
    let starts_with = |name: &str| {
        name.chars()
            .next()
            .is_some_and(|first| first.to_lowercase().eq(letter.to_lowercase()))
    };
    (0..names.len())
        .map(|offset| (start + offset) % names.len())
        .find(|&i| starts_with(names[i]))
}

fn find(list: &'static [Language], code: &str) -> Option<&'static str> {
    list.iter()
        .find(|l| l.code.eq_ignore_ascii_case(code))
        .map(|l| l.code)
}

fn resolve_target(languages: &Languages, code: &str) -> &'static str {
    find(languages.targets, code)
        .or_else(|| find(languages.targets, language::DEFAULT_TARGET))
        .or_else(|| languages.targets.first().map(|l| l.code))
        .unwrap_or(language::DEFAULT_TARGET)
}

fn clamp_splitter(ratio: f32) -> f32 {
    if ratio.is_finite() {
        ratio.clamp(SPLITTER_MIN, SPLITTER_MAX)
    } else {
        0.5
    }
}

/// Display name for a source code reported by the backend.
fn language_name(code: &str) -> String {
    language::source_by_code(code)
        .or_else(|| language::by_code(code))
        .map_or_else(|| code.to_owned(), |l| l.name.to_owned())
}
