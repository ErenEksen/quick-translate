//! Rendering of the popup panel.

use std::fmt;

use iced::widget::{button, column, container, pick_list, row, text, text_editor, toggler};
use iced::{Background, Border, Center, Color, Element, Fill, Padding, Theme};
use quick_translate_core::{BackendKind, Language, MAX_SOURCE_CHARS};

use super::header::Header;
use super::keys::{source_binding, translation_binding};
use super::{Control, Message, Pane, Popup, Split, TranslationStatus, language_name};

const HEADER_TEXT: u32 = 14;
const HEADER_SPACING: f32 = 6.0;
const SMALL_TEXT: u32 = 12;
/// Height of the DeepL/AI switch (logical px); it is twice as wide.
const SWITCH_SIZE: f32 = 12.0;

/// Row between the recently used languages and the full list. Short, so it never widens the
/// selector, which sizes itself to its longest entry.
pub(super) const SEPARATOR: &str = "────────";

/// One row of the source selector: auto-detect, then the recent languages, a separator and
/// every language. A recent language appears twice; the copies differ in `recent` so each
/// row is distinct for the selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SourceOption {
    Auto(String),
    Language { language: Language, recent: bool },
    Separator,
}

impl fmt::Display for SourceOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto(label) => f.write_str(label),
            Self::Language { language, .. } => f.write_str(language.name),
            Self::Separator => f.write_str(SEPARATOR),
        }
    }
}

/// One row of the target selector: the recent languages, a separator, then every language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TargetOption {
    Language { language: Language, recent: bool },
    Separator,
}

impl fmt::Display for TargetOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Language { language, .. } => f.write_str(language.name),
            Self::Separator => f.write_str(SEPARATOR),
        }
    }
}

/// `recent` codes resolved in `list`, then a separator, then all of `list`. No separator
/// without recent languages.
fn with_recent<T>(
    list: &'static [Language],
    recent: &[&'static str],
    option: impl Fn(Language, bool) -> T,
    separator: T,
) -> Vec<T> {
    let recent: Vec<T> = recent
        .iter()
        .filter_map(|code| list.iter().find(|l| l.code == *code))
        .map(|l| option(*l, true))
        .collect();
    let has_recent = !recent.is_empty();
    recent
        .into_iter()
        .chain(has_recent.then_some(separator))
        .chain(list.iter().map(|l| option(*l, false)))
        .collect()
}

impl Popup {
    /// The panel itself. The shell adds placement, resize handles and outside-click handling.
    pub fn view(&self) -> Element<'_, Message> {
        let mut body = column![self.header()].spacing(8).padding(10);
        if let Some(notice) = &self.notice {
            body = body.push(text(notice.as_str()).size(SMALL_TEXT).style(text::warning));
        }
        let body = body.push(self.panes());
        container(body)
            .width(Fill)
            .height(Fill)
            .style(panel_style)
            .into()
    }

    /// Label of the "auto" source entry: "Auto-detect (English)" once DeepL reports a language,
    /// plain "Auto" for backends without detection.
    pub(super) fn auto_label(&self) -> String {
        if !self.backend.reports_detected {
            return "Auto".to_owned();
        }
        match &self.detected {
            Some(code) => format!("Auto-detect ({})", language_name(code)),
            None => "Auto-detect".to_owned(),
        }
    }

    /// Rows of the source selector and the row that is selected.
    pub(super) fn source_options(&self) -> (Vec<SourceOption>, SourceOption) {
        let auto = SourceOption::Auto(self.auto_label());
        let options: Vec<SourceOption> = std::iter::once(auto.clone())
            .chain(with_recent(
                self.languages.sources,
                &self.recent_sources,
                |language, recent| SourceOption::Language { language, recent },
                SourceOption::Separator,
            ))
            .collect();
        // Prefer the copy among the recent rows: the open menu then highlights a row that is
        // visible at the top instead of one far down the list.
        let selected = self
            .source_lang
            .and_then(|code| {
                options.iter().find(|o| {
                    matches!(o, SourceOption::Language { language, .. } if language.code == code)
                })
            })
            .cloned()
            .unwrap_or(auto);
        (options, selected)
    }

    /// Rows of the target selector and the row that is selected.
    pub(super) fn target_options(&self) -> (Vec<TargetOption>, Option<TargetOption>) {
        let options = with_recent(
            self.languages.targets,
            &self.recent_targets,
            |language, recent| TargetOption::Language { language, recent },
            TargetOption::Separator,
        );
        let selected = options.iter().copied().find(|o| {
            matches!(o, TargetOption::Language { language, .. } if language.code == self.target_lang)
        });
        (options, selected)
    }

    fn header(&self) -> Element<'_, Message> {
        let (sources, selected_source) = self.source_options();
        let source_picker = pick_list(sources, Some(selected_source), |o: SourceOption| match o {
            SourceOption::Auto(_) => Message::SourceLanguageSelected(None),
            SourceOption::Language { language, .. } => {
                Message::SourceLanguageSelected(Some(language.code))
            }
            SourceOption::Separator => Message::SelectorClosed(Control::SourceLanguage),
        })
        .text_size(HEADER_TEXT)
        .on_open(Message::SelectorOpened(Control::SourceLanguage));

        let (targets, selected_target) = self.target_options();
        let target_picker = pick_list(targets, selected_target, |o: TargetOption| match o {
            TargetOption::Language { language, .. } => {
                Message::TargetLanguageSelected(language.code)
            }
            TargetOption::Separator => Message::SelectorClosed(Control::TargetLanguage),
        })
        .text_size(HEADER_TEXT)
        .on_open(Message::SelectorOpened(Control::TargetLanguage));

        let swap = button(text("⇄").size(HEADER_TEXT))
            .style(button::secondary)
            .on_press_maybe(self.can_swap().then_some(Message::SwapPressed));
        let copy = button(text("Copy").size(HEADER_TEXT))
            .style(button::primary)
            .on_press_maybe(self.can_copy_all().then_some(Message::CopyPressed));

        let settings = button(text("⚙").size(HEADER_TEXT))
            .style(button::secondary)
            .on_press(Message::OpenSettingsPressed);

        let loading = matches!(
            self.status,
            TranslationStatus::Loading | TranslationStatus::Streaming
        );
        let indicator = text(if loading { "Translating…" } else { "" })
            .size(SMALL_TEXT)
            .style(text::secondary);
        // With both backends set up, the backend name becomes the label of a small switch
        // (on = AI); otherwise it stays plain text.
        let backend: Element<'_, Message> = if self.alternate.is_some() {
            let switch = toggler(self.backend.kind == BackendKind::Openai)
                .label(self.backend.label.as_str())
                .size(SWITCH_SIZE)
                .text_size(SMALL_TEXT)
                .spacing(4)
                .on_toggle(|_| Message::BackendToggled)
                .style(switch_style);
            // A little room after the label so the focus ring does not touch it.
            let switch = container(switch).padding(Padding::ZERO.right(4));
            self.ring(Control::Backend, switch)
        } else {
            text(self.backend.label.as_str())
                .size(SMALL_TEXT)
                .style(text::secondary)
                .into()
        };

        // The language controls wrap when even they do not fit; the right group moves to its
        // own line before it would be cut off, and the indicator only shows where it has room
        // (see `Header`).
        let languages = row![
            self.ring(Control::SourceLanguage, source_picker),
            self.ring(Control::Swap, swap),
            self.ring(Control::TargetLanguage, target_picker),
        ]
        .spacing(HEADER_SPACING)
        .align_y(Center)
        .wrap()
        .vertical_spacing(HEADER_SPACING);
        let actions = row![
            backend,
            self.ring(Control::Copy, copy),
            self.ring(Control::HeaderSettings, settings),
        ]
        .spacing(HEADER_SPACING)
        .align_y(Center);
        Header::new(languages, actions, indicator, HEADER_SPACING).into()
    }

    fn panes(&self) -> Element<'_, Message> {
        let placeholder = if self.capture_pending {
            "Reading selection…"
        } else {
            "Type text to translate"
        };
        let mut source = text_editor(&self.source)
            .id(Pane::Source.widget_id())
            .placeholder(placeholder)
            .height(Fill)
            .key_binding(source_binding);
        if !self.capture_pending {
            source = source.on_action(Message::SourceAction);
        }
        let mut left = column![source].spacing(4);
        if self.truncated {
            left = left.push(
                text(format!(
                    "Selection truncated to {} characters",
                    group_thousands(MAX_SOURCE_CHARS)
                ))
                .size(SMALL_TEXT)
                .style(text::warning),
            );
        }

        let right: Element<'_, Message> = match &self.status {
            TranslationStatus::NotConfigured => {
                let open = button(text("Open settings").size(HEADER_TEXT))
                    .style(button::primary)
                    .on_press(Message::OpenSettingsPressed);
                container(
                    column![
                        text("The translation backend is not configured.").size(HEADER_TEXT),
                        self.ring(Control::Settings, open),
                    ]
                    .spacing(8),
                )
                .padding(8)
                .width(Fill)
                .height(Fill)
                .into()
            }
            status => {
                let placeholder = match status {
                    TranslationStatus::Loading => "Translating…",
                    _ => "",
                };
                let editor = text_editor(&self.translation)
                    .id(Pane::Translation.widget_id())
                    .placeholder(placeholder)
                    .height(Fill)
                    .on_action(Message::TranslationAction)
                    .key_binding(translation_binding);
                let mut right = column![].spacing(4);
                if let TranslationStatus::Failed(error) = status {
                    right = right.push(
                        text(error.to_string())
                            .size(HEADER_TEXT)
                            .style(text::danger),
                    );
                }
                right.push(editor).into()
            }
        };

        Split::new(left, right, self.splitter, Message::SplitterDragged)
            .on_release(Message::SplitterReleased)
            .focused(self.focus == Some(Control::Splitter))
            .into()
    }

    /// Wraps a header control in a border that shows popup-level keyboard focus, since iced
    /// 0.14 buttons and pick lists cannot hold widget focus themselves.
    fn ring<'a>(
        &self,
        control: Control,
        content: impl Into<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        let focused = self.focus == Some(control);
        container(content)
            .padding(2)
            .style(move |theme: &Theme| focus_ring(theme, focused))
            .into()
    }
}

fn focus_ring(theme: &Theme, focused: bool) -> container::Style {
    let color = if focused {
        theme.extended_palette().primary.strong.color
    } else {
        Color::TRANSPARENT
    };
    container::Style {
        border: Border {
            color,
            width: 2.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    }
}

/// The default switch with the secondary text color the plain backend label uses.
fn switch_style(theme: &Theme, status: toggler::Status) -> toggler::Style {
    toggler::Style {
        text_color: text::secondary(theme).color,
        ..toggler::default(theme, status)
    }
}

fn panel_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        text_color: Some(palette.background.base.text),
        background: Some(Background::Color(palette.background.base.color)),
        border: Border {
            color: palette.background.strong.color,
            width: 1.0,
            radius: 10.0.into(),
        },
        // No shadow: tiny-skia draws it outside the damage rect and it piles up into a black halo
        // on the transparent overlay.
        shadow: iced::Shadow::default(),
        snap: true,
    }
}

fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::group_thousands;

    #[test]
    fn thousands_separator() {
        assert_eq!(group_thousands(10_000), "10,000");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1_000_000), "1,000,000");
    }
}
