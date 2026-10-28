use std::sync::Arc;

use futures::StreamExt;
use iced::keyboard::{self, Modifiers, key};
use iced::widget::text_editor::{Action, Binding, Edit, KeyPress, Motion, Status};
use quick_translate_backends::{Chunk, ChunkStream, Error, Request, Translator};
use quick_translate_core::Language;
use tokio_util::sync::CancellationToken;

use super::keys::{source_binding, translation_binding};
use super::*;

static SOURCES: &[Language] = &[
    Language {
        code: "DE",
        name: "German",
    },
    Language {
        code: "EN",
        name: "English",
    },
    Language {
        code: "TR",
        name: "Turkish",
    },
];

static TARGETS: &[Language] = &[
    Language {
        code: "DE",
        name: "German",
    },
    Language {
        code: "EN-GB",
        name: "English (British)",
    },
    Language {
        code: "EN-US",
        name: "English (American)",
    },
    Language {
        code: "TR",
        name: "Turkish",
    },
];

fn deepl() -> BackendInfo {
    BackendInfo {
        label: "DeepL".into(),
        configured: true,
        reports_detected: true,
    }
}

fn options(target: &str, backend: BackendInfo) -> OpenOptions {
    OpenOptions {
        target_lang: target.into(),
        languages: Languages {
            sources: SOURCES,
            targets: TARGETS,
        },
        backend,
        splitter: 0.5,
        theme: Theme::Light,
    }
}

fn popup(target: &str) -> Popup {
    Popup::new(options(target, deepl()))
}

fn started(fx: &[Effect]) -> Option<(TranslationId, Request)> {
    fx.iter().find_map(|e| match e {
        Effect::StartTranslation { id, request } => Some((*id, request.clone())),
        _ => None,
    })
}

fn debounce(fx: &[Effect]) -> Option<u64> {
    fx.iter().find_map(|e| match e {
        Effect::ScheduleDebounce { generation, after } => {
            assert_eq!(*after, DEBOUNCE);
            Some(*generation)
        }
        _ => None,
    })
}

/// Captures `text`, streams `translation` and finishes, with an optional detected language.
fn translated(target: &str, text: &str, detected: Option<&str>, translation: &str) -> Popup {
    let mut p = popup(target);
    let fx = p.update(Message::Captured {
        text: text.into(),
        truncated: false,
    });
    let (id, _) = started(&fx).expect("translation started");
    if let Some(code) = detected {
        let _ = p.update(Message::Chunk {
            id,
            chunk: Chunk::Detected(code.into()),
        });
    }
    let _ = p.update(Message::Chunk {
        id,
        chunk: Chunk::Delta(translation.into()),
    });
    let _ = p.update(Message::TranslationFinished { id });
    assert_eq!(p.status(), &TranslationStatus::Done);
    p
}

fn type_char(p: &mut Popup, c: char) -> Vec<Effect> {
    p.update(Message::SourceAction(Action::Edit(Edit::Insert(c))))
}

// --- capture and placeholder ---

#[test]
fn opens_with_placeholder_and_no_request() {
    let p = popup("TR");
    assert!(p.is_capture_pending());
    assert_eq!(p.source_text(), "");
    assert_eq!(p.status(), &TranslationStatus::Idle);
    assert_eq!(p.current_translation(), None);
    assert_eq!(p.target_lang(), "TR");
}

#[test]
fn edits_are_ignored_while_capture_pending() {
    let mut p = popup("TR");
    assert!(type_char(&mut p, 'x').is_empty());
    assert_eq!(p.source_text(), "");
}

#[test]
fn capture_replaces_placeholder_and_starts_translation() {
    let mut p = popup("EN-US");
    let fx = p.update(Message::Captured {
        text: "Merhaba dünya".into(),
        truncated: false,
    });
    assert!(!p.is_capture_pending());
    assert_eq!(p.source_text(), "Merhaba dünya");
    assert_eq!(p.status(), &TranslationStatus::Loading);
    assert!(fx.contains(&Effect::Focus(Some(Pane::Source))));
    let (id, request) = started(&fx).expect("request");
    assert_eq!(
        request,
        Request {
            text: "Merhaba dünya".into(),
            source_lang: None,
            target_lang: "EN-US".into(),
        }
    );
    assert_eq!(p.current_translation(), Some(id));
}

#[test]
fn empty_capture_focuses_empty_source_without_request() {
    let mut p = popup("TR");
    let fx = p.update(Message::Captured {
        text: String::new(),
        truncated: false,
    });
    assert_eq!(fx, vec![Effect::Focus(Some(Pane::Source))]);
    assert_eq!(p.focused(), Some(Control::SourcePane));
    assert_eq!(p.status(), &TranslationStatus::Idle);
}

#[test]
fn truncation_is_reported() {
    let mut p = popup("TR");
    let _ = p.update(Message::Captured {
        text: "long".into(),
        truncated: true,
    });
    assert!(p.is_truncated());
}

#[test]
fn unknown_target_falls_back_to_english() {
    assert_eq!(popup("XX").target_lang(), "EN-US");
    assert_eq!(popup("tr").target_lang(), "TR");
}

// --- streaming, stale results, errors ---

#[test]
fn deltas_accumulate_and_detected_language_is_shown() {
    let mut p = popup("EN-US");
    let fx = p.update(Message::Captured {
        text: "Merhaba dünya".into(),
        truncated: false,
    });
    let (id, _) = started(&fx).expect("request");
    let _ = p.update(Message::Chunk {
        id,
        chunk: Chunk::Detected("TR".into()),
    });
    for part in ["Hello", " ", "world\nagain"] {
        let _ = p.update(Message::Chunk {
            id,
            chunk: Chunk::Delta(part.into()),
        });
    }
    assert_eq!(p.status(), &TranslationStatus::Streaming);
    assert_eq!(p.translation_text(), "Hello world\nagain");
    assert_eq!(p.translation.text(), "Hello world\nagain");
    assert_eq!(p.detected_lang(), Some("TR"));
    assert_eq!(p.auto_label(), "Auto-detect (Turkish)");
    let _ = p.update(Message::TranslationFinished { id });
    assert_eq!(p.status(), &TranslationStatus::Done);
    assert_eq!(p.current_translation(), None);
}

#[test]
fn backend_without_detection_shows_plain_auto() {
    let backend = BackendInfo {
        label: "AI: llama3".into(),
        configured: true,
        reports_detected: false,
    };
    let mut p = Popup::new(options("TR", backend));
    let fx = p.update(Message::Captured {
        text: "Hello".into(),
        truncated: false,
    });
    let (id, _) = started(&fx).expect("request");
    let _ = p.update(Message::Chunk {
        id,
        chunk: Chunk::Detected("EN".into()),
    });
    assert_eq!(p.detected_lang(), None);
    assert_eq!(p.auto_label(), "Auto");
}

#[test]
fn failures_are_shown_and_not_configured_offers_settings() {
    let mut p = popup("TR");
    let fx = p.update(Message::Captured {
        text: "Hello".into(),
        truncated: false,
    });
    let (id, _) = started(&fx).expect("request");
    let _ = p.update(Message::TranslationFailed {
        id,
        error: Error::QuotaExceeded,
    });
    assert_eq!(p.status(), &TranslationStatus::Failed(Error::QuotaExceeded));
    assert_eq!(p.current_translation(), None);

    let mut p = popup("TR");
    let fx = p.update(Message::Captured {
        text: "Hello".into(),
        truncated: false,
    });
    let (id, _) = started(&fx).expect("request");
    let _ = p.update(Message::TranslationFailed {
        id,
        error: Error::NotConfigured,
    });
    assert_eq!(p.status(), &TranslationStatus::NotConfigured);
}

#[test]
fn unconfigured_backend_sends_nothing_and_opens_settings() {
    let backend = BackendInfo {
        configured: false,
        ..deepl()
    };
    let mut p = Popup::new(options("TR", backend));
    assert_eq!(p.status(), &TranslationStatus::NotConfigured);
    let fx = p.update(Message::Captured {
        text: "Hello".into(),
        truncated: false,
    });
    assert_eq!(started(&fx), None);
    assert_eq!(p.status(), &TranslationStatus::NotConfigured);
    assert_eq!(
        p.update(Message::OpenSettingsPressed),
        vec![Effect::OpenSettings, Effect::Close]
    );
}

#[test]
fn stale_chunks_are_ignored() {
    let mut p = popup("TR");
    let fx = p.update(Message::Captured {
        text: "Hello".into(),
        truncated: false,
    });
    let (old, _) = started(&fx).expect("request");
    let fx = type_char(&mut p, '!');
    assert!(fx.contains(&Effect::CancelTranslation(old)));
    let generation = debounce(&fx).expect("debounce");
    let fx = p.update(Message::DebounceElapsed(generation));
    let (new, request) = started(&fx).expect("new request");
    assert_ne!(old, new);
    assert_eq!(request.text, "!Hello");

    let _ = p.update(Message::Chunk {
        id: old,
        chunk: Chunk::Delta("Merhaba".into()),
    });
    let _ = p.update(Message::TranslationFinished { id: old });
    assert_eq!(p.translation_text(), "");
    assert_eq!(p.status(), &TranslationStatus::Loading);

    let _ = p.update(Message::Chunk {
        id: new,
        chunk: Chunk::Delta("Merhaba!".into()),
    });
    assert_eq!(p.translation_text(), "Merhaba!");
}

// --- debounce and cancellation ---

#[test]
fn continuous_typing_sends_one_request_after_the_pause() {
    let mut p = translated("TR", "Hello", Some("EN"), "Merhaba");
    let mut generations = Vec::new();
    for c in [' ', 'w', 'o'] {
        let fx = type_char(&mut p, c);
        assert_eq!(started(&fx), None, "no request while typing");
        generations.push(debounce(&fx).expect("debounce scheduled"));
    }
    // Timers from superseded keystrokes do nothing.
    for g in &generations[..2] {
        assert!(p.update(Message::DebounceElapsed(*g)).is_empty());
    }
    let fx = p.update(Message::DebounceElapsed(generations[2]));
    let (_, request) = started(&fx).expect("request after pause");
    assert_eq!(request.text, " woHello");
    assert_eq!(p.status(), &TranslationStatus::Loading);
    assert_eq!(p.translation_text(), "", "previous translation replaced");
}

#[test]
fn edit_cancels_in_flight_request() {
    let mut p = popup("TR");
    let fx = p.update(Message::Captured {
        text: "Hello".into(),
        truncated: false,
    });
    let (id, _) = started(&fx).expect("request");
    let fx = type_char(&mut p, 'x');
    assert_eq!(fx[0], Effect::CancelTranslation(id));
    assert_eq!(p.current_translation(), None);
    // A second edit has nothing left to cancel.
    let fx = type_char(&mut p, 'y');
    assert!(!fx.iter().any(|e| matches!(e, Effect::CancelTranslation(_))));
}

#[test]
fn clearing_the_source_cancels_without_new_request() {
    let mut p = translated("TR", "a", None, "b");
    let _ = p.update(Message::SourceAction(Action::SelectAll));
    let fx = p.update(Message::SourceAction(Action::Edit(Edit::Backspace)));
    let fx = p.update(Message::DebounceElapsed(debounce(&fx).expect("debounce")));
    assert_eq!(started(&fx), None);
    assert_eq!(p.status(), &TranslationStatus::Idle);
}

#[test]
fn cursor_movement_does_not_retranslate() {
    let mut p = translated("TR", "Hello", None, "Merhaba");
    assert!(
        p.update(Message::SourceAction(Action::Move(Motion::Left)))
            .is_empty()
    );
}

#[test]
fn unchanged_text_after_debounce_is_not_resent() {
    let mut p = translated("TR", "Hello", None, "Merhaba");
    let _ = type_char(&mut p, 'x');
    let fx = p.update(Message::SourceAction(Action::Edit(Edit::Backspace)));
    // Typed and deleted, but the earlier translation was not cancelled (it had finished).
    let fx = p.update(Message::DebounceElapsed(debounce(&fx).expect("debounce")));
    assert_eq!(started(&fx), None);
    assert_eq!(p.translation_text(), "Merhaba");
}

#[test]
fn closing_cancels_in_flight_request() {
    let mut p = popup("TR");
    let fx = p.update(Message::Captured {
        text: "Hello".into(),
        truncated: false,
    });
    let (id, _) = started(&fx).expect("request");
    assert_eq!(
        p.update(Message::CloseRequested),
        vec![Effect::CancelTranslation(id), Effect::Close]
    );
}

#[test]
fn close_invalidates_pending_debounce() {
    let mut p = translated("TR", "Hello", None, "Merhaba");
    let generation = debounce(&type_char(&mut p, 'x')).expect("debounce");
    let _ = p.update(Message::CloseRequested);
    assert!(p.update(Message::DebounceElapsed(generation)).is_empty());
}

#[test]
fn reset_cancels_and_keeps_ids_increasing() {
    let mut p = popup("TR");
    let fx = p.update(Message::Captured {
        text: "Hello".into(),
        truncated: false,
    });
    let (first, _) = started(&fx).expect("request");
    assert_eq!(
        p.reset(options("DE", deepl())),
        vec![Effect::CancelTranslation(first)]
    );
    assert!(p.is_capture_pending());
    assert_eq!(p.target_lang(), "DE");
    // A late chunk from the previous activation is ignored.
    let _ = p.update(Message::Chunk {
        id: first,
        chunk: Chunk::Delta("x".into()),
    });
    assert_eq!(p.translation_text(), "");
    let fx = p.update(Message::Captured {
        text: "Hi".into(),
        truncated: false,
    });
    let (second, _) = started(&fx).expect("request");
    assert!(second > first);
}

// --- languages and swap ---

#[test]
fn swap_with_detected_source() {
    let mut p = translated("TR", "Hello world", Some("EN"), "Merhaba dünya");
    assert!(p.can_swap());
    let fx = p.update(Message::SwapPressed);
    assert_eq!(p.source_lang(), Some("TR"));
    assert_eq!(p.target_lang(), "EN-US");
    assert_eq!(p.source_text(), "Merhaba dünya");
    assert!(fx.contains(&Effect::PersistTarget("EN-US".into())));
    let (_, request) = started(&fx).expect("new translation");
    assert_eq!(
        request,
        Request {
            text: "Merhaba dünya".into(),
            source_lang: Some("TR".into()),
            target_lang: "EN-US".into(),
        }
    );
}

#[test]
fn swap_with_explicit_source_and_regional_target() {
    let mut p = translated("EN-GB", "Hallo", None, "Hello");
    let _ = p.update(Message::SourceLanguageSelected(Some("DE")));
    let id = p
        .current_translation()
        .expect("re-translation after source change");
    let _ = p.update(Message::Chunk {
        id,
        chunk: Chunk::Delta("Hello".into()),
    });
    let _ = p.update(Message::TranslationFinished { id });
    let fx = p.update(Message::SwapPressed);
    assert_eq!(p.source_lang(), Some("EN"));
    assert_eq!(p.target_lang(), "DE");
    assert_eq!(started(&fx).expect("request").1.text, "Hello");
}

#[test]
fn swap_needs_a_known_source_and_finished_translation() {
    // Auto without detection (OpenAI-compatible).
    let mut p = translated("TR", "Hello", None, "Merhaba");
    assert!(!p.can_swap());
    assert!(p.update(Message::SwapPressed).is_empty());
    assert_eq!(p.target_lang(), "TR");

    // Translation still pending.
    let mut p = popup("TR");
    let _ = p.update(Message::Captured {
        text: "Hello".into(),
        truncated: false,
    });
    assert!(p.update(Message::SwapPressed).is_empty());
}

#[test]
fn target_selection_persists_and_retranslates() {
    let mut p = translated("TR", "Hello", Some("EN"), "Merhaba");
    let fx = p.update(Message::TargetLanguageSelected("DE"));
    assert_eq!(p.target_lang(), "DE");
    assert!(fx.contains(&Effect::PersistTarget("DE".into())));
    assert_eq!(started(&fx).expect("request").1.target_lang, "DE");
    // Selecting the same language again does nothing.
    assert!(p.update(Message::TargetLanguageSelected("DE")).is_empty());
}

#[test]
fn source_selection_cancels_and_retranslates() {
    let mut p = popup("TR");
    let fx = p.update(Message::Captured {
        text: "Hello".into(),
        truncated: false,
    });
    let (id, _) = started(&fx).expect("request");
    let fx = p.update(Message::SourceLanguageSelected(Some("EN")));
    assert_eq!(fx[0], Effect::CancelTranslation(id));
    assert_eq!(
        started(&fx).expect("request").1.source_lang.as_deref(),
        Some("EN")
    );
}

// --- copy and dismiss ---

#[test]
fn ctrl_c_without_selection_copies_full_translation_and_closes() {
    let mut p = translated("EN-US", "Merhaba dünya", Some("TR"), "Hello world");
    assert_eq!(
        p.update(Message::Key(Key::Copy)),
        vec![Effect::CopyToClipboard("Hello world".into()), Effect::Close]
    );
}

#[test]
fn ctrl_c_copies_translation_selection() {
    let mut p = translated("EN-US", "Merhaba dünya", Some("TR"), "Hello world");
    let _ = p.update(Message::TranslationAction(Action::Move(
        Motion::DocumentEnd,
    )));
    let _ = p.update(Message::TranslationAction(Action::Select(Motion::WordLeft)));
    assert_eq!(
        p.update(Message::Key(Key::Copy)),
        vec![Effect::CopyToClipboard("world".into()), Effect::Close]
    );
}

#[test]
fn ctrl_c_copies_source_selection() {
    let mut p = translated("EN-US", "Merhaba dünya", Some("TR"), "Hello world");
    let _ = p.update(Message::SourceAction(Action::Move(Motion::DocumentStart)));
    let _ = p.update(Message::SourceAction(Action::Select(Motion::WordRight)));
    assert_eq!(
        p.update(Message::Key(Key::Copy)),
        vec![Effect::CopyToClipboard("Merhaba".into()), Effect::Close]
    );
}

#[test]
fn ctrl_c_while_pending_does_nothing() {
    let mut p = popup("TR");
    assert!(p.update(Message::Key(Key::Copy)).is_empty());
    let fx = p.update(Message::Captured {
        text: "Hello".into(),
        truncated: false,
    });
    let (id, _) = started(&fx).expect("request");
    assert!(p.update(Message::Key(Key::Copy)).is_empty());
    // A partial streamed translation is not a translation yet.
    let _ = p.update(Message::Chunk {
        id,
        chunk: Chunk::Delta("Mer".into()),
    });
    assert!(p.update(Message::Key(Key::Copy)).is_empty());
    assert_eq!(p.current_translation(), Some(id), "popup stays open");
}

#[test]
fn copy_button_copies_full_translation_and_closes() {
    let mut p = translated("TR", "Hello", None, "Merhaba");
    let _ = p.update(Message::TranslationAction(Action::SelectWord));
    assert_eq!(
        p.update(Message::CopyPressed),
        vec![Effect::CopyToClipboard("Merhaba".into()), Effect::Close]
    );
    let mut p = popup("TR");
    assert!(p.update(Message::CopyPressed).is_empty());
}

#[test]
fn escape_closes_without_copying() {
    let mut p = translated("TR", "Hello", None, "Merhaba");
    assert_eq!(p.update(Message::Key(Key::Escape)), vec![Effect::Close]);
}

#[test]
fn translation_pane_is_read_only() {
    let mut p = translated("TR", "Hello", None, "Merhaba");
    for action in [
        Action::Edit(Edit::Insert('x')),
        Action::Edit(Edit::Paste(Arc::new("y".into()))),
        Action::Edit(Edit::Backspace),
    ] {
        assert!(p.update(Message::TranslationAction(action)).is_empty());
    }
    assert_eq!(p.translation.text(), "Merhaba");
}

// --- keyboard navigation ---

fn tab(p: &mut Popup, forward: bool) -> (Option<Control>, Vec<Effect>) {
    let fx = p.update(Message::Key(if forward { Key::Tab } else { Key::BackTab }));
    (p.focused(), fx)
}

#[test]
fn tab_cycles_through_all_enabled_controls() {
    let mut p = translated("TR", "Hello", Some("EN"), "Merhaba");
    assert_eq!(p.focused(), Some(Control::SourcePane));
    let order = [
        Control::Splitter,
        Control::TranslationPane,
        Control::SourceLanguage,
        Control::Swap,
        Control::TargetLanguage,
        Control::Copy,
        Control::HeaderSettings,
        Control::SourcePane,
    ];
    for expected in order {
        let (focused, fx) = tab(&mut p, true);
        assert_eq!(focused, Some(expected));
        assert_eq!(fx, vec![Effect::Focus(expected.pane())]);
    }
    for expected in order.iter().rev().skip(1) {
        assert_eq!(tab(&mut p, false).0, Some(*expected));
    }
}

#[test]
fn tab_skips_disabled_controls() {
    let mut p = popup("TR");
    // Capture pending: no source pane; nothing translated: no swap or copy.
    let seen: Vec<_> = (0..5)
        .map(|_| tab(&mut p, true).0.expect("focus"))
        .collect();
    assert_eq!(
        seen,
        vec![
            Control::SourceLanguage,
            Control::TargetLanguage,
            Control::HeaderSettings,
            Control::Splitter,
            Control::TranslationPane,
        ]
    );

    let backend = BackendInfo {
        configured: false,
        ..deepl()
    };
    let mut p = Popup::new(options("TR", backend));
    let _ = p.update(Message::Captured {
        text: "Hi".into(),
        truncated: false,
    });
    assert_eq!(tab(&mut p, true).0, Some(Control::Splitter));
    assert_eq!(tab(&mut p, true).0, Some(Control::Settings));
    assert_eq!(
        p.update(Message::Key(Key::Enter)),
        vec![Effect::OpenSettings, Effect::Close]
    );
}

#[test]
fn header_settings_icon_opens_settings_by_keyboard() {
    let mut p = translated("TR", "Hello", Some("EN"), "Merhaba");
    while p.focused() != Some(Control::HeaderSettings) {
        let _ = p.update(Message::Key(Key::Tab));
    }
    assert_eq!(
        p.update(Message::Key(Key::Enter)),
        vec![Effect::OpenSettings, Effect::Close]
    );
}

#[test]
fn target_selector_by_keyboard() {
    let mut p = translated("EN-GB", "Hallo", Some("DE"), "Hello");
    while p.focused() != Some(Control::TargetLanguage) {
        let _ = p.update(Message::Key(Key::Tab));
    }
    let fx = p.update(Message::Key(Key::Down));
    assert_eq!(p.target_lang(), "EN-US");
    assert!(debounce(&fx).is_some());
    let fx = p.update(Message::Key(Key::Down));
    assert_eq!(p.target_lang(), "TR");
    let generation = debounce(&fx).expect("debounce");
    let fx = p.update(Message::Key(Key::Enter));
    assert_eq!(
        fx,
        vec![
            Effect::PersistTarget("TR".into()),
            Effect::StartTranslation {
                id: p.current_translation().expect("request"),
                request: Request {
                    text: "Hallo".into(),
                    source_lang: None,
                    target_lang: "TR".into(),
                },
            },
        ]
    );
    // Enter started it immediately; the earlier timer is now stale.
    assert!(p.update(Message::DebounceElapsed(generation)).is_empty());
    // Bottom of the list stays put; letters jump.
    assert!(p.update(Message::Key(Key::Down)).is_empty());
    let _ = p.update(Message::Key(Key::Char('g')));
    assert_eq!(p.target_lang(), "DE");
    let _ = p.update(Message::Key(Key::Char('e')));
    assert_eq!(p.target_lang(), "EN-GB");
}

#[test]
fn target_chosen_with_arrows_starts_after_debounce() {
    let mut p = translated("TR", "Hello", Some("EN"), "Merhaba");
    let _ = p.update(Message::SelectorOpened(Control::TargetLanguage));
    let fx = p.update(Message::Key(Key::Up));
    assert_eq!(p.target_lang(), "EN-US");
    let fx = p.update(Message::DebounceElapsed(debounce(&fx).expect("debounce")));
    assert_eq!(fx[0], Effect::PersistTarget("EN-US".into()));
    assert_eq!(started(&fx).expect("request").1.target_lang, "EN-US");
}

#[test]
fn source_selector_by_keyboard_includes_auto() {
    let mut p = translated("TR", "Hello", Some("EN"), "Merhaba");
    let _ = p.update(Message::SelectorOpened(Control::SourceLanguage));
    let _ = p.update(Message::Key(Key::Down));
    assert_eq!(p.source_lang(), Some("DE"));
    let _ = p.update(Message::Key(Key::End));
    assert_eq!(p.source_lang(), Some("TR"));
    let _ = p.update(Message::Key(Key::Home));
    assert_eq!(p.source_lang(), None);
}

#[test]
fn buttons_activate_with_enter_and_space() {
    let mut p = translated("TR", "Hello", Some("EN"), "Merhaba");
    while p.focused() != Some(Control::Swap) {
        let _ = p.update(Message::Key(Key::Tab));
    }
    let fx = p.update(Message::Key(Key::Space));
    assert_eq!(p.target_lang(), "EN-US");
    assert!(started(&fx).is_some());

    let mut p = translated("TR", "Hello", None, "Merhaba");
    while p.focused() != Some(Control::Copy) {
        let _ = p.update(Message::Key(Key::Tab));
    }
    assert_eq!(
        p.update(Message::Key(Key::Enter)),
        vec![Effect::CopyToClipboard("Merhaba".into()), Effect::Close]
    );
}

#[test]
fn mouse_interaction_moves_keyboard_focus() {
    let mut p = translated("TR", "Hello", None, "Merhaba");
    let _ = p.update(Message::TranslationAction(Action::Click(
        iced::Point::ORIGIN,
    )));
    assert_eq!(p.focused(), Some(Control::TranslationPane));
    let fx = p.update(Message::SelectorOpened(Control::TargetLanguage));
    assert_eq!(fx, vec![Effect::Focus(None)]);
    assert_eq!(p.focused(), Some(Control::TargetLanguage));
}

// --- splitter ---

#[test]
fn splitter_is_clamped_and_persisted_on_release() {
    let mut p = popup("TR");
    assert!(p.update(Message::SplitterDragged(0.95)).is_empty());
    assert_eq!(p.splitter(), SPLITTER_MAX);
    let _ = p.update(Message::SplitterDragged(0.01));
    assert_eq!(p.splitter(), SPLITTER_MIN);
    let _ = p.update(Message::SplitterDragged(0.6));
    assert_eq!(
        p.update(Message::SplitterReleased),
        vec![Effect::PersistSplitter(0.6)]
    );
}

#[test]
fn splitter_moves_with_arrow_keys() {
    let mut p = popup("TR");
    while p.focused() != Some(Control::Splitter) {
        let _ = p.update(Message::Key(Key::Tab));
    }
    let fx = p.update(Message::Key(Key::Left));
    assert!((p.splitter() - 0.45).abs() < 1e-6);
    assert_eq!(fx, vec![Effect::PersistSplitter(p.splitter())]);
}

#[test]
fn persisted_splitter_is_clamped_on_open() {
    let mut o = options("TR", deepl());
    o.splitter = f32::NAN;
    assert_eq!(Popup::new(o.clone()).splitter(), 0.5);
    o.splitter = 2.0;
    assert_eq!(Popup::new(o).splitter(), SPLITTER_MAX);
}

// --- keys ---

#[test]
fn key_classification() {
    let ctrl = Modifiers::CTRL;
    let c = keyboard::Key::Character("c".into());
    assert_eq!(
        Key::from_press(&c, key::Physical::Code(key::Code::KeyC), ctrl, None),
        Some(Key::Copy)
    );
    // Non-latin layout: the physical C key still copies.
    let cyr = keyboard::Key::Character("с".into());
    assert_eq!(
        Key::from_press(&cyr, key::Physical::Code(key::Code::KeyC), ctrl, None),
        Some(Key::Copy)
    );
    let tab = keyboard::Key::Named(key::Named::Tab);
    let phys = key::Physical::Code(key::Code::Tab);
    assert_eq!(
        Key::from_press(&tab, phys, Modifiers::SHIFT, None),
        Some(Key::BackTab)
    );
    assert_eq!(
        Key::from_press(
            &c,
            key::Physical::Code(key::Code::KeyC),
            Modifiers::empty(),
            Some("c")
        ),
        Some(Key::Char('c'))
    );
    assert_eq!(
        Key::from_press(
            &c,
            key::Physical::Code(key::Code::KeyC),
            Modifiers::ALT,
            Some("c")
        ),
        None
    );
}

fn press(
    key: keyboard::Key,
    code: key::Code,
    modifiers: Modifiers,
    text: Option<&str>,
) -> KeyPress {
    KeyPress {
        key: key.clone(),
        modified_key: key,
        physical_key: key::Physical::Code(code),
        modifiers,
        text: text.map(Into::into),
        status: Status::Focused { is_hovered: false },
    }
}

#[test]
fn editors_leave_popup_keys_alone() {
    let ctrl_c = || {
        press(
            keyboard::Key::Character("c".into()),
            key::Code::KeyC,
            Modifiers::CTRL,
            None,
        )
    };
    let esc = || {
        press(
            keyboard::Key::Named(key::Named::Escape),
            key::Code::Escape,
            Modifiers::empty(),
            None,
        )
    };
    let tab = || {
        press(
            keyboard::Key::Named(key::Named::Tab),
            key::Code::Tab,
            Modifiers::empty(),
            Some("\t"),
        )
    };
    for p in [ctrl_c(), esc(), tab()] {
        assert_eq!(source_binding(p), None);
    }
    for p in [ctrl_c(), esc(), tab()] {
        assert_eq!(translation_binding(p), None);
    }
    let x = || {
        press(
            keyboard::Key::Character("x".into()),
            key::Code::KeyX,
            Modifiers::empty(),
            Some("x"),
        )
    };
    assert_eq!(source_binding(x()), Some(Binding::Insert('x')));
    assert_eq!(translation_binding(x()), None);
    let ctrl_a = press(
        keyboard::Key::Character("a".into()),
        key::Code::KeyA,
        Modifiers::CTRL,
        None,
    );
    assert_eq!(translation_binding(ctrl_a), Some(Binding::SelectAll));
    let paste = press(
        keyboard::Key::Character("v".into()),
        key::Code::KeyV,
        Modifiers::CTRL,
        None,
    );
    assert_eq!(translation_binding(paste), None);
}

// --- translation stream ---

struct Fake(Result<Vec<Result<Chunk, Error>>, Error>);

impl Translator for Fake {
    fn translate(
        &self,
        _req: Request,
        _cancel: CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = quick_translate_backends::Result<ChunkStream>> + Send + '_>,
    > {
        let result = self
            .0
            .clone()
            .map(|items| futures::stream::iter(items).boxed());
        Box::pin(async move { result })
    }
}

fn run(fake: Fake) -> Vec<String> {
    let request = Request {
        text: "Hi".into(),
        source_lang: None,
        target_lang: "TR".into(),
    };
    let stream = translation_stream(
        Arc::new(fake),
        TranslationId(7),
        request,
        CancellationToken::new(),
    );
    futures::executor::block_on(stream.collect::<Vec<_>>())
        .into_iter()
        .map(|m| format!("{m:?}"))
        .collect()
}

#[test]
fn translation_stream_reports_chunks_then_finished() {
    let out = run(Fake(Ok(vec![
        Ok(Chunk::Detected("EN".into())),
        Ok(Chunk::Delta("Merhaba".into())),
    ])));
    assert_eq!(out.len(), 3);
    assert!(out[0].contains("Detected"));
    assert!(out[1].contains("Merhaba"));
    assert!(out[2].starts_with("TranslationFinished"));
}

#[test]
fn translation_stream_stops_at_first_error() {
    let out = run(Fake(Ok(vec![
        Ok(Chunk::Delta("a".into())),
        Err(Error::Timeout),
        Ok(Chunk::Delta("b".into())),
    ])));
    assert_eq!(out.len(), 2);
    assert!(out[1].contains("Timeout"));

    let out = run(Fake(Err(Error::InvalidKey)));
    assert_eq!(out.len(), 1);
    assert!(out[0].contains("InvalidKey"));
}

#[test]
fn ui_effects_become_tasks_and_others_are_returned() {
    assert!(Effect::Focus(None).into_task().is_ok());
    assert!(
        Effect::ScheduleDebounce {
            generation: 1,
            after: DEBOUNCE
        }
        .into_task()
        .is_ok()
    );
    assert_eq!(Effect::Close.into_task().err(), Some(Effect::Close));
}

#[test]
fn view_builds_in_every_state() {
    let mut p = popup("TR");
    let _ = p.view();
    let _ = p.update(Message::Captured {
        text: "Hello".into(),
        truncated: true,
    });
    let _ = p.view();
    let p = translated("TR", "Hello", Some("EN"), "Merhaba");
    let _ = p.view();
    let backend = BackendInfo {
        configured: false,
        ..deepl()
    };
    let _ = Popup::new(options("TR", backend)).view();
}
