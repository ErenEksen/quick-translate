//! Popup-level keyboard handling.
//!
//! Both text editors leave Ctrl+C, Esc and Tab unconsumed (see [`source_binding`] and
//! [`translation_binding`]), so those keys always reach [`subscription`] and the popup decides
//! what they do. Keys pressed while no editor is focused also arrive here and drive the header
//! controls, which iced 0.14 cannot focus by itself.

use iced::keyboard::{self, Modifiers, key};
use iced::widget::text_editor::{Binding, KeyPress};
use iced::{Event, Subscription, event, window};

use super::Message;

/// A key press the popup reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Ctrl+C (layout-independent).
    Copy,
    Escape,
    Tab,
    /// Shift+Tab.
    BackTab,
    Enter,
    Space,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    /// A printable character without Ctrl/Alt, used for jump-to-letter in language selectors.
    Char(char),
}

impl Key {
    /// Classifies a raw key press.
    pub fn from_press(
        key: &keyboard::Key,
        physical_key: key::Physical,
        modifiers: Modifiers,
        text: Option<&str>,
    ) -> Option<Self> {
        if modifiers.command() && key.to_latin(physical_key) == Some('c') {
            return Some(Self::Copy);
        }
        if let keyboard::Key::Named(named) = key {
            return Some(match named {
                key::Named::Escape => Self::Escape,
                key::Named::Tab if modifiers.shift() => Self::BackTab,
                key::Named::Tab => Self::Tab,
                key::Named::Enter => Self::Enter,
                key::Named::Space => Self::Space,
                key::Named::ArrowUp => Self::Up,
                key::Named::ArrowDown => Self::Down,
                key::Named::ArrowLeft => Self::Left,
                key::Named::ArrowRight => Self::Right,
                key::Named::Home => Self::Home,
                key::Named::End => Self::End,
                _ => return None,
            });
        }
        if modifiers.control() || modifiers.alt() || modifiers.logo() {
            return None;
        }
        let c = text?.chars().find(|c| !c.is_control())?;
        Some(if c == ' ' { Self::Space } else { Self::Char(c) })
    }
}

/// Keyboard events no widget consumed, as [`Message::Key`]. Shells batch this with their own
/// subscriptions while the popup is visible.
pub fn subscription() -> Subscription<Message> {
    event::listen_with(on_event)
}

fn on_event(event: Event, status: event::Status, _window: window::Id) -> Option<Message> {
    if status == event::Status::Captured {
        return None;
    }
    let Event::Keyboard(keyboard::Event::KeyPressed {
        key,
        physical_key,
        modifiers,
        text,
        ..
    }) = event
    else {
        return None;
    };
    Key::from_press(&key, physical_key, modifiers, text.as_deref()).map(Message::Key)
}

/// Keys the popup handles itself, so editors must not consume them.
fn is_popup_key(press: &KeyPress) -> bool {
    let copy = press.modifiers.command() && press.key.to_latin(press.physical_key) == Some('c');
    copy || matches!(
        press.key.as_ref(),
        keyboard::Key::Named(key::Named::Escape | key::Named::Tab)
    )
}

/// Key bindings for the editable source pane: iced defaults minus the popup keys.
pub(super) fn source_binding(press: KeyPress) -> Option<Binding<Message>> {
    if is_popup_key(&press) {
        return None;
    }
    Binding::from_key_press(press)
}

/// Key bindings for the read-only translation pane: only movement and selection.
pub(super) fn translation_binding(press: KeyPress) -> Option<Binding<Message>> {
    if is_popup_key(&press) {
        return None;
    }
    match Binding::from_key_press(press)? {
        binding @ (Binding::Move(_)
        | Binding::Select(_)
        | Binding::SelectWord
        | Binding::SelectLine
        | Binding::SelectAll) => Some(binding),
        _ => None,
    }
}
