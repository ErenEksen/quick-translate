//! Rendering of the popup panel.

use std::fmt;

use iced::widget::{button, column, container, pick_list, row, space, text, text_editor};
use iced::{Background, Border, Center, Color, Element, Fill, Theme};
use quick_translate_core::{Language, MAX_SOURCE_CHARS};

use super::keys::{source_binding, translation_binding};
use super::{Control, Message, Pane, Popup, Split, TranslationStatus, language_name};

const HEADER_TEXT: u32 = 14;
const SMALL_TEXT: u32 = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceOption {
    code: Option<&'static str>,
    label: String,
}

impl fmt::Display for SourceOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TargetOption(Language);

impl fmt::Display for TargetOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.name)
    }
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

    fn header(&self) -> Element<'_, Message> {
        let auto = SourceOption {
            code: None,
            label: self.auto_label(),
        };
        let sources: Vec<SourceOption> = std::iter::once(auto.clone())
            .chain(self.languages.sources.iter().map(|l| SourceOption {
                code: Some(l.code),
                label: l.name.to_owned(),
            }))
            .collect();
        let selected_source = sources
            .iter()
            .find(|o| o.code == self.source_lang)
            .cloned()
            .unwrap_or(auto);
        let source_picker = pick_list(sources, Some(selected_source), |o: SourceOption| {
            Message::SourceLanguageSelected(o.code)
        })
        .text_size(HEADER_TEXT)
        .on_open(Message::SelectorOpened(Control::SourceLanguage));

        let targets: Vec<TargetOption> = self
            .languages
            .targets
            .iter()
            .copied()
            .map(TargetOption)
            .collect();
        let selected_target = targets
            .iter()
            .copied()
            .find(|o| o.0.code == self.target_lang);
        let target_picker = pick_list(targets, selected_target, |o: TargetOption| {
            Message::TargetLanguageSelected(o.0.code)
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
        let backend = text(self.backend.label.as_str())
            .size(SMALL_TEXT)
            .style(text::secondary);

        row![
            self.ring(Control::SourceLanguage, source_picker),
            self.ring(Control::Swap, swap),
            self.ring(Control::TargetLanguage, target_picker),
            space::horizontal(),
            indicator,
            backend,
            self.ring(Control::Copy, copy),
            self.ring(Control::HeaderSettings, settings),
        ]
        .spacing(6)
        .align_y(Center)
        .into()
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
