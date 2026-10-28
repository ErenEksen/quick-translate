use quick_translate_backends::{Error, Usage};
use quick_translate_core::{BackendKind, Config};
use quick_translate_ipc::{
    DaemonStatus, SecretString, ShortcutInfo, ShortcutMechanism, SnippetInfo,
};

use super::*;

fn key(value: &str) -> SecretString {
    SecretString::new(value)
}

fn portal(version: u32) -> ShortcutInfo {
    ShortcutInfo {
        mechanism: ShortcutMechanism::Portal,
        label: "System shortcut (portal)".into(),
        portal_version: version,
        trigger: Some("Ctrl+Alt+C".into()),
        can_configure: version >= 2,
        evdev_warning: "reads all keyboard input".into(),
        ..ShortcutInfo::default()
    }
}

fn compositor() -> ShortcutInfo {
    ShortcutInfo {
        mechanism: ShortcutMechanism::Compositor,
        label: "Compositor binding (configure it in your compositor)".into(),
        snippet: Some(SnippetInfo {
            name: "niri".into(),
            instructions: "Add this to ~/.config/niri/config.kdl".into(),
            text: "binds { Ctrl+Alt+C { spawn \"quick-translate\" \"--toggle\"; } }".into(),
        }),
        ..ShortcutInfo::default()
    }
}

fn loaded() -> Loaded {
    Loaded {
        config: Config::default(),
        config_path: "/tmp/qt/config.toml".into(),
        config_error: None,
        keyring_persistent: true,
        deepl_key: None,
        openai_key: None,
        keyring_error: None,
        daemon: Some(DaemonStatus {
            version: "0.1.0".into(),
            pid: 42,
            capabilities: "Layer shell: yes (v4)".into(),
            popup_visible: false,
        }),
        shortcut: Ok(portal(2)),
        dark: false,
    }
}

fn ready_with(loaded: Loaded) -> (Settings, Vec<Effect>) {
    let mut settings = Settings::new();
    let fx = settings.update(Message::Loaded(Box::new(LoadedMessage(loaded))));
    (settings, fx)
}

fn ready() -> Settings {
    let (settings, fx) = ready_with(loaded());
    assert!(fx.is_empty(), "{fx:?}");
    settings
}

fn usage() -> Usage {
    Usage {
        character_count: 120_000,
        character_limit: 1_000_000,
    }
}

fn validate_request(fx: &[Effect]) -> u64 {
    match fx {
        [Effect::ValidateDeepl { request, .. }] => *request,
        other => panic!("expected one ValidateDeepl, got {other:?}"),
    }
}

#[test]
fn input_is_ignored_until_loaded() {
    let mut settings = Settings::new();
    assert!(settings.is_loading());
    assert!(
        settings
            .update(Message::BackendSelected(BackendKind::Openai))
            .is_empty()
    );
    assert!(settings.update(Message::DeeplSavePressed).is_empty());
    let fx = settings.update(Message::Loaded(Box::new(LoadedMessage(loaded()))));
    assert!(fx.is_empty());
    assert!(!settings.is_loading());
    assert_eq!(settings.config().backend, BackendKind::Deepl);
}

#[test]
fn opening_with_a_stored_deepl_key_shows_the_quota() {
    let (mut settings, fx) = ready_with(Loaded {
        deepl_key: Some(key("abc:fx")),
        ..loaded()
    });
    assert!(
        matches!(&fx[..], [Effect::ValidateDeepl { key: k, .. }] if k.expose() == "abc:fx"),
        "{fx:?}"
    );
    assert_eq!(settings.deepl_status(), &DeeplStatus::Checking);
    let request = validate_request(&fx);
    let fx = settings.update(Message::DeeplChecked {
        request,
        result: Ok(DeeplCheck {
            usage: usage(),
            host: "api-free.deepl.com",
        }),
    });
    // A check on open stores nothing.
    assert!(fx.is_empty(), "{fx:?}");
    assert_eq!(settings.deepl_status(), &DeeplStatus::Valid(usage()));
    assert_eq!(format_usage(&usage()), "120,000 / 1,000,000");
}

#[test]
fn simple_controls_save_immediately() {
    let mut settings = ready();
    assert_eq!(
        settings.update(Message::BackendSelected(BackendKind::Openai)),
        [Effect::SaveConfig(ConfigPatch::Backend(
            BackendKind::Openai
        ))]
    );
    assert_eq!(settings.config().backend, BackendKind::Openai);
    assert_eq!(
        settings.update(Message::TargetSelected(Some("TR"))),
        [Effect::SaveConfig(ConfigPatch::Target(Some("TR".into())))]
    );
    assert_eq!(settings.config().target_lang.as_deref(), Some("TR"));
    assert_eq!(
        settings.update(Message::TargetSelected(None)),
        [Effect::SaveConfig(ConfigPatch::Target(None))]
    );
    assert_eq!(
        settings.update(Message::AutostartToggled(false)),
        [Effect::SaveConfig(ConfigPatch::Autostart(false))]
    );
    assert!(!settings.config().autostart);
}

#[test]
fn config_patches_apply_only_their_field() {
    let mut config = Config {
        target_lang: Some("DE".into()),
        ..Config::default()
    };
    config.popup.width = 900.0;
    ConfigPatch::OpenAi {
        base_url: "http://localhost:11434/v1".into(),
        model: "llama3".into(),
        allow_insecure: false,
    }
    .apply(&mut config);
    ConfigPatch::Evdev(true).apply(&mut config);
    ConfigPatch::DeeplHost(Some("api.deepl.com".into())).apply(&mut config);
    assert_eq!(config.openai.base_url, "http://localhost:11434/v1");
    assert_eq!(config.openai.model, "llama3");
    assert!(config.evdev_enabled);
    assert_eq!(config.deepl.cached_host.as_deref(), Some("api.deepl.com"));
    assert_eq!(config.popup.width, 900.0);
    assert_eq!(config.target_lang.as_deref(), Some("DE"));
}

#[test]
fn save_errors_are_reported() {
    let mut settings = ready();
    let _ = settings.update(Message::AutostartToggled(false));
    let _ = settings.update(Message::ConfigSaved(Err(SaveFailure::Reload(
        "The quick-translate daemon is not running".into(),
    ))));
    assert!(matches!(
        settings.save_error(),
        Some(SaveFailure::Reload(_))
    ));
    let _ = settings.update(Message::ConfigSaved(Ok(())));
    assert_eq!(settings.save_error(), None);
}

#[test]
fn get_api_key_opens_the_deepl_key_page() {
    let mut settings = ready();
    assert_eq!(
        settings.update(Message::GetApiKeyPressed),
        [Effect::OpenUrl("https://www.deepl.com/your-account/keys")]
    );
}

#[test]
fn valid_deepl_key_is_validated_then_stored_with_its_host() {
    let mut settings = ready();
    let _ = settings.update(Message::DeeplKeyChanged(key("  new-key:fx ")));
    let fx = settings.update(Message::DeeplSavePressed);
    assert!(
        matches!(&fx[..], [Effect::ValidateDeepl { key: k, .. }] if k.expose() == "new-key:fx"),
        "{fx:?}"
    );
    let request = validate_request(&fx);
    let fx = settings.update(Message::DeeplChecked {
        request,
        result: Ok(DeeplCheck {
            usage: usage(),
            host: "api-free.deepl.com",
        }),
    });
    assert_eq!(
        fx,
        [
            Effect::StoreSecret {
                backend: BackendKind::Deepl,
                key: key("new-key:fx"),
                session_only: false,
            },
            Effect::SaveConfig(ConfigPatch::DeeplHost(Some("api-free.deepl.com".into()))),
        ]
    );
    assert_eq!(settings.deepl_status(), &DeeplStatus::Valid(usage()));
    let fx = settings.update(Message::SecretStored {
        backend: BackendKind::Deepl,
        key: key("new-key:fx"),
        result: Ok(Stored::Keyring),
    });
    assert!(fx.is_empty());
}

#[test]
fn invalid_deepl_key_is_not_stored() {
    let mut settings = ready();
    let _ = settings.update(Message::DeeplKeyChanged(key("bad")));
    let request = validate_request(&settings.update(Message::DeeplSavePressed));
    let fx = settings.update(Message::DeeplChecked {
        request,
        result: Err(Error::InvalidKey),
    });
    assert!(fx.is_empty(), "{fx:?}");
    assert_eq!(settings.deepl_status(), &DeeplStatus::Invalid);
}

#[test]
fn unreachable_deepl_still_stores_the_key() {
    let mut settings = ready();
    let _ = settings.update(Message::DeeplKeyChanged(key("k")));
    let request = validate_request(&settings.update(Message::DeeplSavePressed));
    let fx = settings.update(Message::DeeplChecked {
        request,
        result: Err(Error::Timeout),
    });
    assert_eq!(
        fx,
        [
            Effect::StoreSecret {
                backend: BackendKind::Deepl,
                key: key("k"),
                session_only: false,
            },
            Effect::SaveConfig(ConfigPatch::DeeplHost(None)),
        ]
    );
    assert!(matches!(
        settings.deepl_status(),
        DeeplStatus::CheckFailed(reason) if reason.contains("timed out")
    ));
}

#[test]
fn stale_deepl_results_are_ignored() {
    let mut settings = ready();
    let _ = settings.update(Message::DeeplKeyChanged(key("first")));
    let first = validate_request(&settings.update(Message::DeeplSavePressed));
    let _ = settings.update(Message::DeeplKeyChanged(key("second")));
    let second = validate_request(&settings.update(Message::DeeplSavePressed));
    assert_ne!(first, second);
    let fx = settings.update(Message::DeeplChecked {
        request: first,
        result: Err(Error::InvalidKey),
    });
    assert!(fx.is_empty());
    assert_eq!(settings.deepl_status(), &DeeplStatus::Checking);
    let fx = settings.update(Message::DeeplChecked {
        request: second,
        result: Ok(DeeplCheck {
            usage: usage(),
            host: "api.deepl.com",
        }),
    });
    assert!(
        matches!(&fx[0], Effect::StoreSecret { key: k, .. } if k.expose() == "second"),
        "{fx:?}"
    );
}

#[test]
fn clearing_the_deepl_key_deletes_it() {
    let (mut settings, _) = ready_with(Loaded {
        deepl_key: Some(key("old")),
        ..loaded()
    });
    let _ = settings.update(Message::DeeplKeyChanged(key("")));
    assert_eq!(
        settings.update(Message::DeeplSavePressed),
        [
            Effect::DeleteSecret(BackendKind::Deepl),
            Effect::SaveConfig(ConfigPatch::DeeplHost(None)),
        ]
    );
    assert_eq!(settings.deepl_status(), &DeeplStatus::Missing);
    // Nothing stored any more: saving an empty field again does nothing.
    assert!(settings.update(Message::DeeplSavePressed).is_empty());
}

#[test]
fn without_a_keyring_saving_offers_session_only_use() {
    let (mut settings, _) = ready_with(Loaded {
        keyring_persistent: false,
        ..loaded()
    });
    let _ = settings.update(Message::DeeplKeyChanged(key("k:fx")));
    assert!(settings.update(Message::DeeplSavePressed).is_empty());
    let prompt = settings.session_prompt().expect("prompt");
    assert_eq!(prompt.backend, BackendKind::Deepl);
    assert_eq!(prompt.reason, SessionReason::NoProvider);

    // Cancel keeps nothing.
    assert!(settings.update(Message::SessionOnlyCancelled).is_empty());
    assert!(settings.session_prompt().is_none());

    let _ = settings.update(Message::DeeplSavePressed);
    let fx = settings.update(Message::SessionOnlyConfirmed);
    let request = validate_request(&fx);
    let fx = settings.update(Message::DeeplChecked {
        request,
        result: Ok(DeeplCheck {
            usage: usage(),
            host: "api-free.deepl.com",
        }),
    });
    assert_eq!(
        fx[0],
        Effect::StoreSecret {
            backend: BackendKind::Deepl,
            key: key("k:fx"),
            session_only: true,
        }
    );
    // Accepted once per window: the OpenAI key goes straight to the session.
    let _ = settings.update(Message::OpenAiKeyChanged(key("sk-1")));
    let fx = settings.update(Message::OpenAiSavePressed);
    assert!(
        fx.contains(&Effect::StoreSecret {
            backend: BackendKind::Openai,
            key: key("sk-1"),
            session_only: true,
        }),
        "{fx:?}"
    );
}

#[test]
fn keyring_failure_offers_session_only_for_that_key() {
    let mut settings = ready();
    let fx = settings.update(Message::SecretStored {
        backend: BackendKind::Openai,
        key: key("sk-2"),
        result: Err(StoreFailure::Keyring(
            "The keyring prompt was dismissed".into(),
        )),
    });
    assert!(fx.is_empty());
    let prompt = settings.session_prompt().expect("prompt");
    assert!(matches!(&prompt.reason, SessionReason::KeyringFailed(r) if r.contains("dismissed")));
    assert_eq!(
        settings.update(Message::SessionOnlyConfirmed),
        [Effect::StoreSecret {
            backend: BackendKind::Openai,
            key: key("sk-2"),
            session_only: true,
        }]
    );
}

#[test]
fn session_store_failure_is_shown() {
    let mut settings = ready();
    let _ = settings.update(Message::SecretStored {
        backend: BackendKind::Deepl,
        key: key("k"),
        result: Err(StoreFailure::Daemon(
            "The quick-translate daemon is not running".into(),
        )),
    });
    assert!(matches!(
        settings.deepl_status(),
        DeeplStatus::Error(e) if e.contains("not running")
    ));
}

#[test]
fn openai_save_writes_config_and_key() {
    let mut settings = ready();
    let _ = settings.update(Message::OpenAiUrlChanged(
        "http://localhost:11434/v1".into(),
    ));
    let _ = settings.update(Message::OpenAiModelChanged(" llama3 ".into()));
    let fx = settings.update(Message::OpenAiSavePressed);
    // Loopback http needs no confirmation; an empty key that was never stored is left alone.
    assert_eq!(
        fx,
        [Effect::SaveConfig(ConfigPatch::OpenAi {
            base_url: "http://localhost:11434/v1".into(),
            model: "llama3".into(),
            allow_insecure: false,
        })]
    );
    assert_eq!(settings.openai_status(), &OpenAiStatus::Saved);

    let _ = settings.update(Message::OpenAiKeyChanged(key("sk-abc")));
    let fx = settings.update(Message::OpenAiSavePressed);
    assert_eq!(
        fx[1],
        Effect::StoreSecret {
            backend: BackendKind::Openai,
            key: key("sk-abc"),
            session_only: false,
        }
    );
    let _ = settings.update(Message::SecretStored {
        backend: BackendKind::Openai,
        key: key("sk-abc"),
        result: Ok(Stored::Keyring),
    });
    // Unchanged key: only the config is saved.
    assert_eq!(settings.update(Message::OpenAiSavePressed).len(), 1);
    // Cleared key: deleted.
    let _ = settings.update(Message::OpenAiKeyChanged(key("")));
    let fx = settings.update(Message::OpenAiSavePressed);
    assert_eq!(fx[1], Effect::DeleteSecret(BackendKind::Openai));
}

#[test]
fn insecure_remote_url_is_refused_until_confirmed() {
    let mut settings = ready();
    let _ = settings.update(Message::OpenAiUrlChanged("http://example.com/v1".into()));
    assert!(settings.update(Message::OpenAiSavePressed).is_empty());
    assert_eq!(settings.insecure_prompt(), Some(InsecureAction::Save));

    assert!(settings.update(Message::InsecureCancelled).is_empty());
    assert_eq!(settings.insecure_prompt(), None);
    assert!(settings.update(Message::OpenAiSavePressed).is_empty());

    let fx = settings.update(Message::InsecureConfirmed);
    assert_eq!(
        fx,
        [Effect::SaveConfig(ConfigPatch::OpenAi {
            base_url: "http://example.com/v1".into(),
            model: Config::default().openai.model,
            allow_insecure: true,
        })]
    );
    assert_eq!(settings.insecure_prompt(), None);
    assert!(settings.config().openai.allow_insecure);

    // A different insecure host needs a new confirmation.
    let _ = settings.update(Message::OpenAiUrlChanged(
        "http://192.168.1.5:8080/v1".into(),
    ));
    assert!(settings.update(Message::OpenAiSavePressed).is_empty());
    assert_eq!(settings.insecure_prompt(), Some(InsecureAction::Save));
    // Going back to the confirmed URL does not.
    let _ = settings.update(Message::OpenAiUrlChanged("http://example.com/v1".into()));
    assert_eq!(settings.update(Message::OpenAiSavePressed).len(), 1);
}

#[test]
fn https_and_loopback_urls_clear_the_insecure_flag() {
    let mut settings = ready();
    let _ = settings.update(Message::OpenAiUrlChanged("http://example.com/v1".into()));
    let _ = settings.update(Message::OpenAiSavePressed);
    let _ = settings.update(Message::InsecureConfirmed);
    for url in ["https://openrouter.ai/api/v1", "http://127.0.0.1:8080/v1"] {
        let _ = settings.update(Message::OpenAiUrlChanged(url.into()));
        let fx = settings.update(Message::OpenAiSavePressed);
        assert!(
            matches!(
                &fx[..],
                [Effect::SaveConfig(ConfigPatch::OpenAi {
                    allow_insecure: false,
                    ..
                })]
            ),
            "{url}: {fx:?}"
        );
    }
}

#[test]
fn invalid_endpoint_fields_are_reported() {
    let mut settings = ready();
    let _ = settings.update(Message::OpenAiUrlChanged("not a url".into()));
    assert!(settings.update(Message::OpenAiSavePressed).is_empty());
    assert!(
        matches!(settings.openai_status(), OpenAiStatus::Invalid(e) if e.contains("Invalid URL"))
    );
    let _ = settings.update(Message::OpenAiUrlChanged("ftp://example.com".into()));
    assert!(settings.update(Message::OpenAiTestPressed).is_empty());
    assert!(matches!(settings.openai_status(), OpenAiStatus::Invalid(_)));
    let _ = settings.update(Message::OpenAiUrlChanged(
        "https://api.openai.com/v1".into(),
    ));
    let _ = settings.update(Message::OpenAiModelChanged("  ".into()));
    assert!(settings.update(Message::OpenAiSavePressed).is_empty());
    assert_eq!(
        settings.openai_status(),
        &OpenAiStatus::Invalid("Enter a model name".into())
    );
}

#[test]
fn test_connection_uses_the_fields_and_reports_the_result() {
    let mut settings = ready();
    let _ = settings.update(Message::TargetSelected(Some("DE")));
    let _ = settings.update(Message::OpenAiUrlChanged(
        "http://localhost:11434/v1".into(),
    ));
    let _ = settings.update(Message::OpenAiModelChanged("llama3".into()));
    let fx = settings.update(Message::OpenAiTestPressed);
    let request = match &fx[..] {
        [
            Effect::TestOpenAi {
                request,
                base_url,
                model,
                key,
                allow_insecure: false,
                target_lang,
            },
        ] => {
            assert_eq!(base_url, "http://localhost:11434/v1");
            assert_eq!(model, "llama3");
            assert!(key.is_empty());
            assert_eq!(target_lang, "DE");
            *request
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(settings.openai_status(), &OpenAiStatus::Testing);
    let _ = settings.update(Message::OpenAiTested {
        request,
        result: Ok("Hallo\n".into()),
    });
    assert_eq!(
        settings.openai_status(),
        &OpenAiStatus::TestOk("Hallo".into())
    );

    let fx = settings.update(Message::OpenAiTestPressed);
    let Effect::TestOpenAi { request, .. } = fx[0] else {
        panic!("{fx:?}");
    };
    let _ = settings.update(Message::OpenAiTested {
        request,
        result: Err(Error::Service("model 'llama9' not found".into())),
    });
    assert_eq!(
        settings.openai_status(),
        &OpenAiStatus::TestFailed("model 'llama9' not found".into())
    );
}

#[test]
fn test_connection_to_insecure_url_asks_first() {
    let mut settings = ready();
    let _ = settings.update(Message::OpenAiUrlChanged("http://example.com/v1".into()));
    assert!(settings.update(Message::OpenAiTestPressed).is_empty());
    assert_eq!(settings.insecure_prompt(), Some(InsecureAction::Test));
    let fx = settings.update(Message::InsecureConfirmed);
    assert!(
        matches!(
            &fx[..],
            [Effect::TestOpenAi {
                allow_insecure: true,
                ..
            }]
        ),
        "{fx:?}"
    );
}

#[test]
fn portal_v2_change_opens_the_portal_and_v1_shows_instructions() {
    let mut settings = ready();
    assert_eq!(
        settings.update(Message::ChangeShortcutPressed),
        [Effect::ConfigureShortcut]
    );
    let _ = settings.update(Message::ShortcutConfigured(Err("portal busy".into())));
    assert_eq!(
        settings.shortcut_notice(),
        Some(&ShortcutNotice::Error("portal busy".into()))
    );

    let (mut settings, _) = ready_with(Loaded {
        shortcut: Ok(portal(1)),
        ..loaded()
    });
    assert!(settings.update(Message::ChangeShortcutPressed).is_empty());
    assert_eq!(
        settings.shortcut_notice(),
        Some(&ShortcutNotice::Instructions)
    );
}

#[test]
fn trigger_updates_live() {
    let mut settings = ready();
    let _ = settings.update(Message::TriggerChanged(Some("Super+T".into())));
    let ShortcutState::Ready(info) = settings.shortcut() else {
        panic!("not ready");
    };
    assert_eq!(info.trigger.as_deref(), Some("Super+T"));
    let _ = settings.update(Message::TriggerChanged(Some(String::new())));
    let ShortcutState::Ready(info) = settings.shortcut() else {
        panic!("not ready");
    };
    assert_eq!(info.trigger, None);
}

#[test]
fn compositor_snippet_copy() {
    let (mut settings, _) = ready_with(Loaded {
        shortcut: Ok(compositor()),
        ..loaded()
    });
    let fx = settings.update(Message::CopySnippetPressed);
    assert_eq!(
        fx,
        [Effect::CopyToClipboard(
            compositor().snippet.expect("snippet").text
        )]
    );
    assert_eq!(settings.shortcut_notice(), Some(&ShortcutNotice::Copied));
    // Compositor bindings cannot be changed from here.
    assert!(settings.update(Message::ChangeShortcutPressed).is_empty());
}

#[test]
fn evdev_needs_confirmation_and_reports_permission_errors() {
    let mut settings = ready();
    assert!(settings.update(Message::EvdevToggled(true)).is_empty());
    assert!(settings.evdev_prompt());
    assert!(!settings.config().evdev_enabled);

    assert!(settings.update(Message::EvdevCancelled).is_empty());
    assert!(!settings.evdev_prompt());

    let _ = settings.update(Message::EvdevToggled(true));
    assert_eq!(
        settings.update(Message::EvdevConfirmed),
        [Effect::SetEvdev(true)]
    );
    // Toggling while the daemon works is ignored.
    assert!(settings.update(Message::EvdevToggled(true)).is_empty());
    let message = "no permission to read keyboard input devices (/dev/input)";
    let fx = settings.update(Message::EvdevSet {
        enabled: true,
        result: Err(message.into()),
    });
    assert!(fx.is_empty(), "the opt-in must not be saved: {fx:?}");
    assert_eq!(settings.evdev_error(), Some(message));
    assert!(!settings.config().evdev_enabled);

    let _ = settings.update(Message::EvdevToggled(true));
    let _ = settings.update(Message::EvdevConfirmed);
    let fx = settings.update(Message::EvdevSet {
        enabled: true,
        result: Ok(()),
    });
    assert_eq!(
        fx,
        [
            Effect::SaveConfig(ConfigPatch::Evdev(true)),
            Effect::RefreshDaemon
        ]
    );
    assert!(settings.config().evdev_enabled);
    assert_eq!(settings.evdev_error(), None);

    // Turning it off needs no confirmation and is saved even if the daemon fails.
    assert_eq!(
        settings.update(Message::EvdevToggled(false)),
        [Effect::SetEvdev(false)]
    );
    let fx = settings.update(Message::EvdevSet {
        enabled: false,
        result: Err("The quick-translate daemon is not running".into()),
    });
    assert_eq!(fx, [Effect::SaveConfig(ConfigPatch::Evdev(false))]);
    assert!(!settings.config().evdev_enabled);
}

#[test]
fn daemon_unavailable_and_refresh() {
    let (mut settings, _) = ready_with(Loaded {
        daemon: None,
        shortcut: Err("The quick-translate daemon is not running".into()),
        ..loaded()
    });
    assert!(matches!(settings.shortcut(), ShortcutState::Unavailable(_)));
    assert_eq!(
        settings.update(Message::RefreshPressed),
        [Effect::RefreshDaemon]
    );
    let _ = settings.update(Message::DaemonRefreshed {
        daemon: loaded().daemon,
        shortcut: Ok(compositor()),
    });
    assert!(matches!(settings.shortcut(), ShortcutState::Ready(i) if i.snippet.is_some()));
}

#[test]
fn raise_request_focuses_the_window() {
    let mut settings = ready();
    assert_eq!(settings.update(Message::RaiseRequested), [Effect::Raise]);
}

#[test]
fn effects_and_messages_never_show_keys() {
    let effect = Effect::StoreSecret {
        backend: BackendKind::Deepl,
        key: key("super-secret-key"),
        session_only: false,
    };
    assert!(!format!("{effect:?}").contains("super-secret-key"));
    let message = Message::DeeplKeyChanged(key("super-secret-key"));
    assert!(!format!("{message:?}").contains("super-secret-key"));
    let mut settings = ready();
    let _ = settings.update(message);
    assert!(!format!("{settings:?}").contains("super-secret-key"));
}

#[test]
fn views_build_in_every_state() {
    let _ = Settings::new().view();
    for shortcut in [Ok(portal(1)), Ok(compositor()), Err("down".into())] {
        let (mut settings, _) = ready_with(Loaded {
            shortcut,
            keyring_persistent: false,
            config_error: Some("expected `=`".into()),
            ..loaded()
        });
        let _ = settings.update(Message::OpenAiUrlChanged("http://example.com/v1".into()));
        let _ = settings.update(Message::OpenAiSavePressed);
        let _ = settings.update(Message::EvdevToggled(true));
        let _ = settings.update(Message::DeeplKeyChanged(key("k")));
        let _ = settings.update(Message::DeeplSavePressed);
        let _ = settings.view();
    }
}
