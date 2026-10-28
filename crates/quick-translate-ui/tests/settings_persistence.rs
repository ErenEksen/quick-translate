//! Settings persist across restarts (task 8.6): the update logic and its effects run against a
//! temp `config.toml`, `gnome-keyring-daemon` and a mock daemon, all on a PRIVATE D-Bus bus
//! without service activation, and a DeepL/OpenAI mock server. A second, fresh settings
//! instance then reloads everything. Nothing touches the user's session bus or keyring.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use quick_translate_backends::DeeplEndpoints;
use quick_translate_core::{BackendKind, ConfigStore, LoadOutcome};
use quick_translate_ipc::{
    BusClaim, DaemonCommand, DaemonStatus, ShortcutInfo, ShortcutMechanism, claim_bus_name,
    emit_settings_requested, emit_shortcut_changed,
};
use quick_translate_secrets::{BackendId, SecretStore};
use quick_translate_ui::settings::{
    DeeplStatus, Effect, InsecureAction, Message, OpenAiStatus, Runtime, Settings, ShortcutState,
};
use tokio::sync::mpsc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DEEPL_KEY: &str = "0123-settings-test-key:fx";
const OPENAI_KEY: &str = "sk-settings-test-key";

struct Bus {
    dir: tempfile::TempDir,
    address: String,
    children: Vec<Child>,
}

fn have(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

impl Bus {
    /// A private bus with no service activation, or `None` (skip) without `dbus-daemon`.
    fn start() -> Option<Self> {
        if !have("dbus-daemon") {
            eprintln!("dbus-daemon not installed, skipping");
            return None;
        }
        let dir = tempfile::Builder::new()
            .prefix("qtset")
            .tempdir_in("/tmp")
            .unwrap();
        for sub in ["home", "data", "config", "cache", "run"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.path().join("run"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let conf = dir.path().join("bus.conf");
        std::fs::write(
            &conf,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:dir={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                dir.path().display()
            ),
        )
        .unwrap();
        let mut bus = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", conf.display()))
            .args(["--nofork", "--print-address"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir.path().join("home"))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(bus.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_owned();
        assert!(
            address.contains(dir.path().to_str().unwrap()),
            "bus must be private: {address}"
        );
        Some(Self {
            dir,
            address,
            children: vec![bus],
        })
    }

    /// Adds `gnome-keyring-daemon` with an unlocked `login` collection; `false` (skip) when
    /// it is not installed.
    async fn start_keyring(&mut self) -> bool {
        if !have("gnome-keyring-daemon") {
            eprintln!("gnome-keyring-daemon not installed, skipping");
            return false;
        }
        let d = self.dir.path();
        let mut child = Command::new("gnome-keyring-daemon")
            .args(["--foreground", "--components=secrets", "--unlock"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", d.join("home"))
            .env("XDG_DATA_HOME", d.join("data"))
            .env("XDG_CONFIG_HOME", d.join("config"))
            .env("XDG_CACHE_HOME", d.join("cache"))
            .env("XDG_RUNTIME_DIR", d.join("run"))
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"test-password").unwrap();
        drop(stdin);
        self.children.push(child);
        let cnx = self.connect().await;
        let dbus = zbus::fdo::DBusProxy::new(&cnx).await.unwrap();
        let name = zbus::names::BusName::try_from("org.freedesktop.secrets").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !dbus.name_has_owner(name.clone()).await.unwrap() {
            assert!(Instant::now() < deadline, "keyring did not start");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        true
    }

    async fn connect(&self) -> zbus::Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    fn config_path(&self) -> std::path::PathBuf {
        self.dir.path().join("config/quick-translate/config.toml")
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        for child in self.children.iter_mut().rev() {
            child.kill().ok();
            child.wait().ok();
        }
    }
}

/// What the mock daemon saw.
#[derive(Debug, Default)]
struct Seen {
    reloads: usize,
    /// `(backend, key)` per `SetSessionSecret`.
    session: Vec<(BackendKind, String)>,
    evdev: Vec<bool>,
}

fn mock_shortcut() -> ShortcutInfo {
    ShortcutInfo {
        mechanism: ShortcutMechanism::Portal,
        label: "System shortcut (portal)".into(),
        portal_version: 2,
        trigger: Some("Ctrl+Alt+C".into()),
        can_configure: true,
        evdev_warning: "reads all keyboard input".into(),
        ..ShortcutInfo::default()
    }
}

/// Claims the daemon name on `bus` and answers the settings calls.
async fn mock_daemon(bus: &Bus) -> (zbus::Connection, Arc<Mutex<Seen>>) {
    let conn = bus.connect().await;
    let (tx, mut rx) = mpsc::channel(16);
    assert_eq!(claim_bus_name(&conn, tx).await.unwrap(), BusClaim::Owner);
    let seen = Arc::new(Mutex::new(Seen::default()));
    let record = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Some(command) = rx.recv().await {
            let mut seen = record.lock().unwrap();
            match command {
                DaemonCommand::Status { reply } => {
                    let _ = reply.send(DaemonStatus {
                        version: "9.9.9".into(),
                        pid: 4242,
                        capabilities: "Layer shell: yes (v4)".into(),
                        popup_visible: false,
                    });
                }
                DaemonCommand::ShortcutInfo { reply } => {
                    let _ = reply.send(mock_shortcut());
                }
                DaemonCommand::ReloadConfig { reply } => {
                    seen.reloads += 1;
                    let _ = reply.send(Ok(()));
                }
                DaemonCommand::SetSessionSecret { backend, key } => {
                    seen.session.push((backend, key.expose().to_owned()));
                }
                DaemonCommand::SetEvdevEnabled { enabled, reply } => {
                    seen.evdev.push(enabled);
                    let _ = reply.send(Ok(()));
                }
                DaemonCommand::ConfigureShortcut { reply, .. } => {
                    let _ = reply.send(Ok(()));
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    });
    (conn, seen)
}

async fn api_mock() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/usage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"character_count":120000,"character_limit":1000000}"#),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"Merhaba\"}}]}\n\ndata: [DONE]\n\n",
                ),
        )
        .mount(&server)
        .await;
    server
}

async fn runtime(bus: &Bus, api: &MockServer) -> Runtime {
    let url = url::Url::parse(&api.uri()).unwrap();
    Runtime::with_connection(bus.config_path(), Some(bus.connect().await))
        .await
        .with_deepl_endpoints(DeeplEndpoints {
            free: url.clone(),
            pro: url,
        })
}

/// Feeds `message` and every message the effects produce back into `settings`.
async fn drive(settings: &mut Settings, runtime: &Runtime, message: Message) -> Vec<Effect> {
    let mut queue = VecDeque::from([message]);
    let mut all = Vec::new();
    while let Some(message) = queue.pop_front() {
        for effect in settings.update(message) {
            all.push(effect.clone());
            if let Some(reply) = runtime.execute(effect).await {
                queue.push_back(reply);
            }
        }
    }
    all
}

async fn open(runtime: &Runtime) -> Settings {
    let mut settings = Settings::new();
    drive(&mut settings, runtime, runtime.load().await).await;
    settings
}

fn assert_no_key_material(path: &Path) {
    let text = std::fs::read_to_string(path).unwrap();
    for key in [DEEPL_KEY, OPENAI_KEY] {
        assert!(!text.contains(key), "key in {}:\n{text}", path.display());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_control_persists_across_restarts() {
    let Some(mut bus) = Bus::start() else {
        return;
    };
    if !bus.start_keyring().await {
        return;
    }
    let (daemon_conn, seen) = mock_daemon(&bus).await;
    let api = api_mock().await;
    let rt = runtime(&bus, &api).await;

    let mut s = open(&rt).await;
    assert!(!s.is_loading());
    assert_eq!(s.config().backend, BackendKind::Deepl);
    assert!(
        matches!(s.shortcut(), ShortcutState::Ready(i) if i.trigger.as_deref() == Some("Ctrl+Alt+C"))
    );

    drive(&mut s, &rt, Message::BackendSelected(BackendKind::Openai)).await;
    drive(&mut s, &rt, Message::TargetSelected(Some("TR"))).await;
    drive(&mut s, &rt, Message::AutostartToggled(false)).await;

    drive(
        &mut s,
        &rt,
        Message::DeeplKeyChanged(quick_translate_ipc::SecretString::new(DEEPL_KEY)),
    )
    .await;
    let fx = drive(&mut s, &rt, Message::DeeplSavePressed).await;
    assert!(
        fx.iter().any(|e| matches!(
            e,
            Effect::StoreSecret {
                session_only: false,
                ..
            }
        )),
        "{fx:?}"
    );
    assert!(matches!(s.deepl_status(), DeeplStatus::Valid(u) if u.character_count == 120_000));

    let openai_base = format!("{}/v1", api.uri());
    drive(&mut s, &rt, Message::OpenAiUrlChanged(openai_base.clone())).await;
    drive(&mut s, &rt, Message::OpenAiModelChanged("llama3".into())).await;
    drive(
        &mut s,
        &rt,
        Message::OpenAiKeyChanged(quick_translate_ipc::SecretString::new(OPENAI_KEY)),
    )
    .await;
    drive(&mut s, &rt, Message::OpenAiTestPressed).await;
    assert_eq!(s.openai_status(), &OpenAiStatus::TestOk("Merhaba".into()));
    drive(&mut s, &rt, Message::OpenAiSavePressed).await;
    assert_eq!(s.openai_status(), &OpenAiStatus::Saved);

    drive(&mut s, &rt, Message::EvdevToggled(true)).await;
    assert!(s.evdev_prompt());
    drive(&mut s, &rt, Message::EvdevConfirmed).await;
    assert!(s.config().evdev_enabled);
    assert_eq!(s.save_error(), None);

    // The live trigger and raise signals arrive as messages.
    let mut signals = rt.signals().await;
    emit_shortcut_changed(&daemon_conn, Some("Super+T"))
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(5), signals.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message, Message::TriggerChanged(Some("Super+T".into())));
    drive(&mut s, &rt, message).await;
    assert!(
        matches!(s.shortcut(), ShortcutState::Ready(i) if i.trigger.as_deref() == Some("Super+T"))
    );
    emit_settings_requested(&daemon_conn).await.unwrap();
    let message = tokio::time::timeout(Duration::from_secs(5), signals.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message, Message::RaiseRequested);
    drop(s);
    drop(rt);

    // Restart: a fresh config store, keyring client and settings window see everything.
    let config = match ConfigStore::new(bus.config_path()).load() {
        LoadOutcome::Loaded(config) => config,
        other => panic!("{other:?}"),
    };
    assert_eq!(config.backend, BackendKind::Openai);
    assert_eq!(config.target_lang.as_deref(), Some("TR"));
    assert!(!config.autostart);
    assert!(config.evdev_enabled);
    assert_eq!(config.openai.base_url, openai_base);
    assert_eq!(config.openai.model, "llama3");
    assert!(
        !config.openai.allow_insecure,
        "loopback needs no confirmation"
    );
    assert_eq!(
        config.deepl.cached_host.as_deref(),
        Some("api-free.deepl.com")
    );
    assert_no_key_material(&bus.config_path());

    let secrets = SecretStore::connect_to(&bus.address).await;
    assert!(secrets.is_persistent());
    assert_eq!(
        secrets.get(BackendId::Deepl).await,
        Ok(Some(DEEPL_KEY.into()))
    );
    assert_eq!(
        secrets.get(BackendId::Openai).await,
        Ok(Some(OPENAI_KEY.into()))
    );

    let rt = runtime(&bus, &api).await;
    let s = open(&rt).await;
    assert_eq!(s.config(), &config);
    // Opening with a stored key validates it and shows the quota.
    assert!(matches!(s.deepl_status(), DeeplStatus::Valid(u) if u.character_limit == 1_000_000));

    let seen = seen.lock().unwrap();
    assert!(seen.reloads >= 6, "{seen:?}");
    assert_eq!(seen.evdev, [true]);
    // Persistent saves only clear the daemon's session keys.
    assert!(
        seen.session.iter().all(|(_, key)| key.is_empty()),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_keyring_keys_live_in_the_daemon_for_the_session() {
    let Some(bus) = Bus::start() else {
        return;
    };
    let (_daemon, seen) = mock_daemon(&bus).await;
    let api = api_mock().await;
    let rt = runtime(&bus, &api).await;
    let mut s = open(&rt).await;

    drive(
        &mut s,
        &rt,
        Message::DeeplKeyChanged(quick_translate_ipc::SecretString::new(DEEPL_KEY)),
    )
    .await;
    assert!(
        drive(&mut s, &rt, Message::DeeplSavePressed)
            .await
            .is_empty()
    );
    assert!(s.session_prompt().is_some(), "the user must be told first");
    drive(&mut s, &rt, Message::SessionOnlyConfirmed).await;
    assert!(matches!(s.deepl_status(), DeeplStatus::Valid(_)));
    assert_eq!(
        seen.lock().unwrap().session,
        [(BackendKind::Deepl, DEEPL_KEY.to_owned())]
    );
    drop(s);
    drop(rt);

    // A restarted settings process has no key: it lived only in (mock) daemon memory.
    let secrets = SecretStore::connect_to(&bus.address).await;
    assert!(!secrets.is_persistent());
    assert_eq!(secrets.get(BackendId::Deepl).await, Ok(None));
    assert_no_key_material(&bus.config_path());
    let rt = runtime(&bus, &api).await;
    let s = open(&rt).await;
    assert_eq!(s.deepl_status(), &DeeplStatus::Missing);
}

#[tokio::test(flavor = "multi_thread")]
async fn insecure_endpoint_persists_only_after_confirmation() {
    let Some(bus) = Bus::start() else {
        return;
    };
    let (_daemon, _seen) = mock_daemon(&bus).await;
    let api = api_mock().await;
    let rt = runtime(&bus, &api).await;
    let mut s = open(&rt).await;

    drive(
        &mut s,
        &rt,
        Message::OpenAiUrlChanged("http://example.com/v1".into()),
    )
    .await;
    assert!(
        drive(&mut s, &rt, Message::OpenAiSavePressed)
            .await
            .is_empty()
    );
    assert_eq!(s.insecure_prompt(), Some(InsecureAction::Save));
    let config = ConfigStore::new(bus.config_path()).load().config().clone();
    assert_eq!(
        config.openai.base_url, "https://api.openai.com/v1",
        "nothing saved yet"
    );

    drive(&mut s, &rt, Message::InsecureConfirmed).await;
    let config = ConfigStore::new(bus.config_path()).load().config().clone();
    assert_eq!(config.openai.base_url, "http://example.com/v1");
    assert!(config.openai.allow_insecure);
}

#[tokio::test(flavor = "multi_thread")]
async fn saves_keep_daemon_written_values_and_unknown_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("quick-translate/config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "future-flag = 1\n[popup]\nwidth = 900.0\n").unwrap();
    let rt = Runtime::with_connection(path.clone(), None).await;
    let mut s = open(&rt).await;
    assert_eq!(s.config().popup.width, 900.0);

    // The daemon persists a new popup size while settings is open.
    let mut store = ConfigStore::new(&path);
    let mut config = store.load().config().clone();
    config.popup.width = 1000.0;
    store.save(&config).unwrap();

    drive(&mut s, &rt, Message::AutostartToggled(false)).await;
    // No daemon (no bus): written, but not applied.
    assert!(matches!(
        s.save_error(),
        Some(quick_translate_ui::settings::SaveFailure::Reload(_))
    ));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("future-flag = 1"), "{text}");
    let config = ConfigStore::new(&path).load().config().clone();
    assert_eq!(config.popup.width, 1000.0);
    assert!(!config.autostart);
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_config_is_replaced_on_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "backend = [\n").unwrap();
    let rt = Runtime::with_connection(path.clone(), None).await;
    let mut s = open(&rt).await;
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "backend = [\n");
    drive(&mut s, &rt, Message::BackendSelected(BackendKind::Openai)).await;
    let config = match ConfigStore::new(&path).load() {
        LoadOutcome::Loaded(config) => config,
        other => panic!("{other:?}"),
    };
    assert_eq!(config.backend, BackendKind::Openai);
}
