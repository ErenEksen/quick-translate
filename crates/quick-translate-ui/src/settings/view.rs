//! Rendering of the settings window.

use std::fmt;

use iced::widget::{
    button, center, column, container, pick_list, radio, row, rule, scrollable, space, text,
    text_input, toggler,
};
use iced::{Background, Border, Center, Color, Element, Fill, Font, Theme};
use quick_translate_backends::EndpointError;
use quick_translate_core::BackendKind;
use quick_translate_core::language::{self, Language};
use quick_translate_ipc::{SecretString, ShortcutInfo, ShortcutMechanism};

use super::{
    DeeplStatus, InsecureAction, Message, OpenAiStatus, SaveFailure, SessionPrompt, SessionReason,
    Settings, ShortcutNotice, ShortcutState, Stored, format_usage,
};

const TITLE: u32 = 18;
const BODY: u32 = 14;
const SMALL: u32 = 12;

/// Shown before the user confirms the input-device fallback when the daemon sent no text.
const EVDEV_WARNING_FALLBACK: &str = "The input-device fallback reads your keyboard devices \
directly. While it is on, Quick Translate can see everything you type, in every application.";

#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetChoice {
    code: Option<&'static str>,
    label: String,
}

impl fmt::Display for TargetChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

impl Settings {
    pub fn view(&self) -> Element<'_, Message> {
        if self.is_loading() {
            return center(text("Loading settings…").size(BODY)).into();
        }
        let mut page = column![].spacing(16).padding(20).max_width(760);
        for notice in self.notices() {
            page = page.push(notice);
        }
        page = page
            .push(self.translation_section())
            .push(self.deepl_section())
            .push(self.openai_section())
            .push(self.shortcut_section())
            .push(self.startup_section())
            .push(self.diagnostics_section());
        scrollable(container(page).center_x(Fill))
            .height(Fill)
            .into()
    }

    fn notices(&self) -> Vec<Element<'_, Message>> {
        let mut out = Vec::new();
        if let Some(error) = &self.config_error {
            out.push(notice(
                format!(
                    "{} could not be read ({error}). Defaults are shown; saving any setting \
                     replaces the file.",
                    self.config_path.display()
                ),
                Tone::Warning,
            ));
        }
        match &self.save_error {
            Some(SaveFailure::Write(error)) => {
                out.push(notice(
                    format!("Could not save settings: {error}"),
                    Tone::Danger,
                ));
            }
            Some(SaveFailure::Reload(error)) => out.push(notice(
                format!("Saved. The daemon has not applied it yet: {error}"),
                Tone::Warning,
            )),
            None => {}
        }
        if !self.keyring_persistent {
            out.push(notice(
                "No keyring (Secret Service provider) is running. API keys cannot be stored \
                 persistently and can only be used for this session."
                    .to_owned(),
                Tone::Warning,
            ));
        }
        if let Some(error) = &self.keyring_error {
            out.push(notice(
                format!("Could not read the keyring: {error}"),
                Tone::Warning,
            ));
        }
        out
    }

    fn translation_section(&self) -> Element<'_, Message> {
        let selected = Some(self.config.backend);
        let backends = row![
            radio(
                "DeepL",
                BackendKind::Deepl,
                selected,
                Message::BackendSelected
            )
            .text_size(BODY),
            radio(
                "OpenAI-compatible",
                BackendKind::Openai,
                selected,
                Message::BackendSelected
            )
            .text_size(BODY),
        ]
        .spacing(24);

        let system = language::default_target_from_env();
        let mut choices = vec![TargetChoice {
            code: None,
            label: format!("System language ({})", target_name(system)),
        }];
        choices.extend(language::target_languages().iter().map(|l| TargetChoice {
            code: Some(l.code),
            label: l.name.to_owned(),
        }));
        let current = self
            .config
            .target_lang
            .as_ref()
            .map(|_| self.config.effective_target_lang());
        let selected = choices.iter().find(|c| c.code == current).cloned();
        let target = pick_list(choices, selected, |c: TargetChoice| {
            Message::TargetSelected(c.code)
        })
        .text_size(BODY);

        section(
            "Translation",
            column![label("Backend"), backends, label("Translate into"), target,].spacing(8),
        )
    }

    fn deepl_section(&self) -> Element<'_, Message> {
        let checking = self.deepl_status == DeeplStatus::Checking;
        let key = text_input("DeepL API key", self.deepl_key.expose())
            .secure(!self.deepl_show)
            .on_input(|s| Message::DeeplKeyChanged(SecretString::new(s)))
            .on_submit(Message::DeeplSavePressed)
            .size(BODY);
        let show = toggler(self.deepl_show)
            .label("Show")
            .text_size(SMALL)
            .on_toggle(Message::DeeplShowKey);
        let buttons = row![
            button(text("Save key").size(BODY))
                .style(button::primary)
                .on_press_maybe((!checking).then_some(Message::DeeplSavePressed)),
            button(text("Get API key").size(BODY))
                .style(button::secondary)
                .on_press(Message::GetApiKeyPressed),
        ]
        .spacing(8);

        let mut body = column![
            row![key, show].spacing(8).align_y(Center),
            buttons,
            deepl_status_line(&self.deepl_status),
        ]
        .spacing(8);
        if let Some(note) = self.deepl_note {
            body = body.push(stored_note(note));
        }
        if let Some(error) = &self.url_error {
            body = body.push(text(error.as_str()).size(SMALL).style(text::danger));
        }
        if let Some(prompt) = self
            .session_prompt
            .as_ref()
            .filter(|p| p.backend == BackendKind::Deepl)
        {
            body = body.push(session_prompt(prompt));
        }
        section("DeepL", body)
    }

    fn openai_section(&self) -> Element<'_, Message> {
        let busy = self.openai_status == OpenAiStatus::Testing;
        let url = text_input("https://api.openai.com/v1", &self.openai_url)
            .on_input(Message::OpenAiUrlChanged)
            .size(BODY);
        let model = text_input("gpt-4o-mini", &self.openai_model)
            .on_input(Message::OpenAiModelChanged)
            .size(BODY);
        let key = text_input("Optional for local servers", self.openai_key.expose())
            .secure(!self.openai_show)
            .on_input(|s| Message::OpenAiKeyChanged(SecretString::new(s)))
            .on_submit(Message::OpenAiSavePressed)
            .size(BODY);
        let show = toggler(self.openai_show)
            .label("Show")
            .text_size(SMALL)
            .on_toggle(Message::OpenAiShowKey);
        let buttons = row![
            button(text("Save").size(BODY))
                .style(button::primary)
                .on_press(Message::OpenAiSavePressed),
            button(text("Test connection").size(BODY))
                .style(button::secondary)
                .on_press_maybe((!busy).then_some(Message::OpenAiTestPressed)),
        ]
        .spacing(8);

        let mut body = column![
            label("Base URL"),
            url,
            label("Model"),
            model,
            label("API key"),
            row![key, show].spacing(8).align_y(Center),
            buttons,
        ]
        .spacing(8);
        if let Some(action) = self.insecure_prompt {
            body = body.push(insecure_prompt(action));
        }
        if let Some(line) = openai_status_line(&self.openai_status) {
            body = body.push(line);
        }
        if let Some(note) = self.openai_note {
            body = body.push(stored_note(note));
        }
        if let Some(prompt) = self
            .session_prompt
            .as_ref()
            .filter(|p| p.backend == BackendKind::Openai)
        {
            body = body.push(session_prompt(prompt));
        }
        section("OpenAI-compatible", body)
    }

    fn shortcut_section(&self) -> Element<'_, Message> {
        let info = match &self.shortcut {
            ShortcutState::Ready(info) => info,
            ShortcutState::Unavailable(reason) => {
                return section(
                    "Shortcut",
                    column![
                        text(reason.as_str()).size(BODY).style(text::secondary),
                        button(text("Refresh").size(BODY))
                            .style(button::secondary)
                            .on_press(Message::RefreshPressed),
                    ]
                    .spacing(8),
                );
            }
        };
        let mut body = column![
            row![
                label("Active mechanism:"),
                text(info.label.as_str()).size(BODY)
            ]
            .spacing(6)
        ]
        .spacing(8);
        match info.mechanism {
            ShortcutMechanism::Portal => {
                let trigger = info.trigger.as_deref().unwrap_or("Not assigned");
                body = body.push(
                    row![
                        label("Shortcut:"),
                        text(trigger).size(BODY).font(Font::MONOSPACE),
                        space::horizontal(),
                        button(text("Change shortcut").size(BODY))
                            .style(button::secondary)
                            .on_press(Message::ChangeShortcutPressed),
                    ]
                    .spacing(6)
                    .align_y(Center),
                );
                if self.shortcut_notice == Some(ShortcutNotice::Instructions) {
                    body = body.push(
                        text(format!(
                            "Your desktop's shortcut portal (version {}) cannot open its \
                             shortcut editor from here. Open your system shortcut settings \
                             and change \"Translate the selected text\" for Quick Translate.",
                            info.portal_version
                        ))
                        .size(SMALL),
                    );
                }
            }
            ShortcutMechanism::Compositor => body = body.push(snippet_view(info)),
        }
        match &self.shortcut_notice {
            Some(ShortcutNotice::Copied) => {
                body = body.push(
                    text("Copied to the clipboard")
                        .size(SMALL)
                        .style(text::success),
                );
            }
            Some(ShortcutNotice::Error(error)) => {
                body = body.push(text(error.as_str()).size(SMALL).style(text::danger));
            }
            Some(ShortcutNotice::Instructions) | None => {}
        }
        body = body.push(rule::horizontal(1)).push(self.evdev_view(info));
        section("Shortcut", body)
    }

    fn evdev_view<'a>(&'a self, info: &'a ShortcutInfo) -> Element<'a, Message> {
        let on = self.config.evdev_enabled || self.evdev_prompt;
        let toggle = toggler(on)
            .label("Input-device fallback (reads keyboards directly)")
            .text_size(BODY)
            .on_toggle_maybe((!self.evdev_busy).then_some(Message::EvdevToggled));
        let mut body = column![
            toggle,
            text("Detects Ctrl+Alt+C from /dev/input when no portal or compositor binding works.")
                .size(SMALL)
                .style(text::secondary),
        ]
        .spacing(6);
        if self.config.evdev_enabled && info.evdev_active {
            body = body.push(text("Active").size(SMALL).style(text::success));
        }
        if let Some(error) = self.evdev_error.as_deref().or(info.evdev_error.as_deref()) {
            body = body.push(text(error).size(SMALL).style(text::danger));
        }
        if self.evdev_prompt {
            let warning = if info.evdev_warning.is_empty() {
                EVDEV_WARNING_FALLBACK
            } else {
                info.evdev_warning.as_str()
            };
            body = body.push(confirm_box(
                warning,
                "I understand, turn it on",
                Message::EvdevConfirmed,
                Message::EvdevCancelled,
            ));
        }
        body.into()
    }

    fn startup_section(&self) -> Element<'_, Message> {
        section(
            "Startup",
            toggler(self.config.autostart)
                .label("Start on login")
                .text_size(BODY)
                .on_toggle(Message::AutostartToggled),
        )
    }

    fn diagnostics_section(&self) -> Element<'_, Message> {
        let daemon = match &self.daemon {
            Some(status) => format!("running (pid {}, version {})", status.pid, status.version),
            None => "not running".to_owned(),
        };
        let shortcut = match &self.shortcut {
            ShortcutState::Ready(info) if info.portal_version > 0 => {
                format!("{} (GlobalShortcuts v{})", info.label, info.portal_version)
            }
            ShortcutState::Ready(info) => info.label.clone(),
            ShortcutState::Unavailable(reason) => reason.clone(),
        };
        let keyring = if self.keyring_persistent {
            "Secret Service (persistent)"
        } else {
            "not available (session-only keys)"
        };
        let capabilities = self
            .daemon
            .as_ref()
            .map_or("unknown (the daemon is not running)", |s| {
                s.capabilities.as_str()
            });
        let mut body = column![
            fact("Daemon", daemon),
            fact("Shortcut", shortcut),
            fact("Keyring", keyring.to_owned()),
            fact("Config file", self.config_path.display().to_string()),
            label("Capabilities"),
            code_box(capabilities),
        ]
        .spacing(6);
        body = body.push(
            button(text("Refresh").size(BODY))
                .style(button::secondary)
                .on_press(Message::RefreshPressed),
        );
        section("Diagnostics", body)
    }
}

fn snippet_view(info: &ShortcutInfo) -> Element<'_, Message> {
    let Some(snippet) = &info.snippet else {
        return text("Bind `quick-translate --toggle` to a key in your compositor.")
            .size(BODY)
            .into();
    };
    column![
        text(format!("{} binding", snippet.name)).size(BODY),
        text(snippet.instructions.as_str()).size(SMALL),
        code_box(&snippet.text),
        button(text("Copy").size(BODY))
            .style(button::primary)
            .on_press(Message::CopySnippetPressed),
    ]
    .spacing(6)
    .into()
}

fn deepl_status_line(status: &DeeplStatus) -> Element<'_, Message> {
    let (line, style): (String, fn(&Theme) -> text::Style) = match status {
        DeeplStatus::Missing => ("No API key saved.".into(), text::secondary),
        DeeplStatus::Unchecked => (String::new(), text::secondary),
        DeeplStatus::Checking => ("Checking the key…".into(), text::secondary),
        DeeplStatus::Valid(usage) => (
            format!(
                "Valid key · {} characters used this period",
                format_usage(usage)
            ),
            text::success,
        ),
        DeeplStatus::Invalid => ("Invalid API key".into(), text::danger),
        DeeplStatus::CheckFailed(reason) => (
            format!("The key could not be checked: {reason}"),
            text::warning,
        ),
        DeeplStatus::Error(error) => (error.clone(), text::danger),
    };
    text(line).size(SMALL).style(style).into()
}

fn openai_status_line(status: &OpenAiStatus) -> Option<Element<'_, Message>> {
    let (line, style): (String, fn(&Theme) -> text::Style) = match status {
        OpenAiStatus::Idle => return None,
        OpenAiStatus::Invalid(error) => (error.clone(), text::danger),
        OpenAiStatus::Saved => ("Saved.".into(), text::success),
        OpenAiStatus::Testing => ("Testing the connection…".into(), text::secondary),
        OpenAiStatus::TestOk(reply) => (
            format!("Connection works. The model replied: \"{reply}\""),
            text::success,
        ),
        OpenAiStatus::TestFailed(error) => (error.clone(), text::danger),
        OpenAiStatus::Error(error) => (error.clone(), text::danger),
    };
    Some(text(line).size(SMALL).style(style).into())
}

fn stored_note(stored: Stored) -> Element<'static, Message> {
    let line = match stored {
        Stored::Keyring => "Saved in the keyring.",
        Stored::Session => "Saved for this session only. Enter it again after the daemon restarts.",
        Stored::Deleted => "Key removed.",
    };
    text(line).size(SMALL).style(text::secondary).into()
}

fn session_prompt(prompt: &SessionPrompt) -> Element<'_, Message> {
    let reason = match &prompt.reason {
        SessionReason::NoProvider => {
            "No keyring (Secret Service provider) is running, so this key cannot be stored \
             persistently."
                .to_owned()
        }
        SessionReason::KeyringFailed(error) => {
            format!("The keyring did not store the key: {error}.")
        }
    };
    let message = format!(
        "{reason} You can use it for this session only: the daemon keeps it in memory until \
         it exits, and you will have to enter it again after a restart."
    );
    confirm_owned(
        message,
        "Use for this session only",
        Message::SessionOnlyConfirmed,
        Message::SessionOnlyCancelled,
    )
}

fn insecure_prompt(_action: InsecureAction) -> Element<'static, Message> {
    confirm_owned(
        EndpointError::NeedsConfirmation.to_string(),
        "Use unencrypted connection",
        Message::InsecureConfirmed,
        Message::InsecureCancelled,
    )
}

fn confirm_box<'a>(
    message: &'a str,
    accept: &'static str,
    on_accept: Message,
    on_cancel: Message,
) -> Element<'a, Message> {
    container(
        column![
            text(message).size(SMALL),
            row![
                button(text(accept).size(BODY))
                    .style(button::danger)
                    .on_press(on_accept),
                button(text("Cancel").size(BODY))
                    .style(button::secondary)
                    .on_press(on_cancel),
            ]
            .spacing(8),
        ]
        .spacing(8),
    )
    .padding(10)
    .width(Fill)
    .style(|theme: &Theme| tone_style(theme, Tone::Warning))
    .into()
}

fn confirm_owned(
    message: String,
    accept: &'static str,
    on_accept: Message,
    on_cancel: Message,
) -> Element<'static, Message> {
    container(
        column![
            text(message).size(SMALL),
            row![
                button(text(accept).size(BODY))
                    .style(button::primary)
                    .on_press(on_accept),
                button(text("Cancel").size(BODY))
                    .style(button::secondary)
                    .on_press(on_cancel),
            ]
            .spacing(8),
        ]
        .spacing(8),
    )
    .padding(10)
    .width(Fill)
    .style(|theme: &Theme| tone_style(theme, Tone::Warning))
    .into()
}

fn section<'a>(title: &'a str, body: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(column![text(title).size(TITLE), body.into()].spacing(12))
        .padding(16)
        .width(Fill)
        .style(card_style)
        .into()
}

fn label(content: &str) -> Element<'_, Message> {
    text(content).size(SMALL).style(text::secondary).into()
}

fn fact(name: &'static str, value: String) -> Element<'static, Message> {
    row![
        text(name).size(SMALL).style(text::secondary).width(110),
        text(value).size(SMALL),
    ]
    .spacing(8)
    .into()
}

fn code_box(content: &str) -> Element<'_, Message> {
    container(text(content).size(SMALL).font(Font::MONOSPACE))
        .padding(8)
        .width(Fill)
        .style(code_style)
        .into()
}

#[derive(Debug, Clone, Copy)]
enum Tone {
    Warning,
    Danger,
}

fn notice(message: String, tone: Tone) -> Element<'static, Message> {
    container(text(message).size(SMALL))
        .padding(10)
        .width(Fill)
        .style(move |theme: &Theme| tone_style(theme, tone))
        .into()
}

fn tone_style(theme: &Theme, tone: Tone) -> container::Style {
    let palette = theme.extended_palette();
    let pair = match tone {
        Tone::Warning => palette.warning.weak,
        Tone::Danger => palette.danger.weak,
    };
    container::Style {
        text_color: Some(pair.text),
        background: Some(Background::Color(pair.color)),
        border: Border {
            radius: 6.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

fn card_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.weak.color)),
        border: Border {
            color: palette.background.strong.color,
            width: 1.0,
            radius: 10.0.into(),
        },
        ..container::Style::default()
    }
}

fn code_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.base.color)),
        border: Border {
            color: Color {
                a: 0.6,
                ..palette.background.strong.color
            },
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    }
}

fn target_name(code: &str) -> &'static str {
    language::target_by_code(code).map_or("English", |l: Language| l.name)
}
