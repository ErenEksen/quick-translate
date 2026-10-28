//! Portal flow against a mock xdg-desktop-portal on a private session bus.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::StreamExt;
use quick_translate_core::{APP_ID, Capabilities, Compositor};
use quick_translate_platform::shortcut::{
    self, Activation, ConfigureError, Mechanism, PREFERRED_TRIGGER, PortalShortcuts, SHORTCUT_ID,
    SnippetFormat, ToggleCommand, portal,
};
use zbus::{
    Connection, ObjectServer, fdo,
    message::Header,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value},
};

const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
const GS_IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
const STEP: Duration = Duration::from_secs(10);

/// A private `dbus-daemon` without service activation, so no real portal can start on it.
struct Bus {
    child: Child,
    address: String,
    _dir: tempfile::TempDir,
}

impl Bus {
    fn spawn() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("bus.conf");
        std::fs::write(
            &config,
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
        let mut child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--print-address", "--nofork"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon must be installed");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Self {
            child,
            address: address.trim().to_owned(),
            _dir: dir,
        }
    }

    async fn connect(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug, Default)]
struct State {
    version: u32,
    registry_fails: bool,
    cancel_bind: bool,
    trigger: String,
    /// Portal calls in order, as `"<sender> <method>"`.
    calls: Vec<String>,
    registered: Vec<String>,
    /// `(id, description, preferred_trigger)` per BindShortcuts call.
    bound: Vec<(String, String, Option<String>)>,
    configure_tokens: Vec<Option<String>>,
    sessions: Vec<String>,
    closed: Vec<String>,
}

type Shared = Arc<Mutex<State>>;

fn sender_id(header: &Header<'_>) -> String {
    header
        .sender()
        .map(|s| s.trim_start_matches(':').replace('.', "_"))
        .unwrap_or_default()
}

fn log(state: &Shared, header: &Header<'_>, method: &str) {
    let sender = header.sender().map(|s| s.to_string()).unwrap_or_default();
    state
        .lock()
        .unwrap()
        .calls
        .push(format!("{sender} {method}"));
}

fn opt_string(options: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    options
        .get(key)
        .and_then(|v| v.downcast_ref::<String>().ok())
}

fn request_path(header: &Header<'_>, options: &HashMap<String, OwnedValue>) -> OwnedObjectPath {
    let token = opt_string(options, "handle_token").unwrap_or_else(|| "t".into());
    ObjectPath::try_from(format!(
        "{DESKTOP_PATH}/request/{}/{token}",
        sender_id(header)
    ))
    .unwrap()
    .into()
}

async fn respond(
    conn: &Connection,
    path: &OwnedObjectPath,
    code: u32,
    results: HashMap<&str, Value<'_>>,
) -> fdo::Result<()> {
    conn.emit_signal(
        None::<()>,
        path.as_ref(),
        "org.freedesktop.portal.Request",
        "Response",
        &(code, results),
    )
    .await
    .map_err(|e| fdo::Error::Failed(e.to_string()))
}

fn shortcuts_value(trigger: &str) -> Value<'static> {
    let info: HashMap<String, Value<'static>> = HashMap::from([
        (
            "description".to_owned(),
            Value::from("Translate the selected text".to_owned()),
        ),
        (
            "trigger_description".to_owned(),
            Value::from(trigger.to_owned()),
        ),
    ]);
    Value::from(vec![(SHORTCUT_ID.to_owned(), info)])
}

struct Registry(Shared);

#[zbus::interface(name = "org.freedesktop.host.portal.Registry")]
impl Registry {
    async fn register(
        &self,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        log(&self.0, &header, "Register");
        let mut state = self.0.lock().unwrap();
        if state.registry_fails {
            return Err(fdo::Error::Failed("registry disabled in this test".into()));
        }
        state.registered.push(app_id);
        Ok(())
    }
}

struct Shortcuts(Shared);

#[zbus::interface(name = "org.freedesktop.portal.GlobalShortcuts")]
impl Shortcuts {
    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        let mut state = self.0.lock().unwrap();
        state.calls.push("? Get version".into());
        state.version
    }

    async fn create_session(
        &self,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> fdo::Result<OwnedObjectPath> {
        log(&self.0, &header, "CreateSession");
        let request = request_path(&header, &options);
        let token = opt_string(&options, "session_handle_token").unwrap_or_else(|| "s".into());
        let session = format!("{DESKTOP_PATH}/session/{}/{token}", sender_id(&header));
        server
            .at(
                session.as_str(),
                MockSession(self.0.clone(), session.clone()),
            )
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        self.0.lock().unwrap().sessions.push(session.clone());
        respond(
            conn,
            &request,
            0,
            HashMap::from([("session_handle", Value::from(session))]),
        )
        .await?;
        Ok(request)
    }

    async fn bind_shortcuts(
        &self,
        _session: OwnedObjectPath,
        shortcuts: Vec<(String, HashMap<String, OwnedValue>)>,
        _parent_window: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<OwnedObjectPath> {
        log(&self.0, &header, "BindShortcuts");
        let request = request_path(&header, &options);
        let (cancel, trigger) = {
            let mut state = self.0.lock().unwrap();
            for (id, info) in &shortcuts {
                state.bound.push((
                    id.clone(),
                    opt_string(info, "description").unwrap_or_default(),
                    opt_string(info, "preferred_trigger"),
                ));
            }
            (state.cancel_bind, state.trigger.clone())
        };
        if cancel {
            respond(conn, &request, 1, HashMap::new()).await?;
        } else {
            respond(
                conn,
                &request,
                0,
                HashMap::from([("shortcuts", shortcuts_value(&trigger))]),
            )
            .await?;
        }
        Ok(request)
    }

    async fn list_shortcuts(
        &self,
        _session: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<OwnedObjectPath> {
        log(&self.0, &header, "ListShortcuts");
        let request = request_path(&header, &options);
        let (cancel, trigger) = {
            let state = self.0.lock().unwrap();
            (state.cancel_bind, state.trigger.clone())
        };
        let shortcuts = if cancel {
            Value::from(Vec::<(String, HashMap<String, Value<'_>>)>::new())
        } else {
            shortcuts_value(&trigger)
        };
        respond(conn, &request, 0, HashMap::from([("shortcuts", shortcuts)])).await?;
        Ok(request)
    }

    async fn configure_shortcuts(
        &self,
        _session: OwnedObjectPath,
        _parent_window: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        log(&self.0, &header, "ConfigureShortcuts");
        let mut state = self.0.lock().unwrap();
        if state.version < 2 {
            return Err(fdo::Error::UnknownMethod(
                "ConfigureShortcuts needs version 2".into(),
            ));
        }
        state
            .configure_tokens
            .push(opt_string(&options, "activation_token"));
        Ok(())
    }
}

struct MockSession(Shared, String);

#[zbus::interface(name = "org.freedesktop.portal.Session")]
impl MockSession {
    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        1
    }

    async fn close(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> fdo::Result<()> {
        log(&self.0, &header, "Session.Close");
        self.0.lock().unwrap().closed.push(self.1.clone());
        server.remove::<MockSession, _>(self.1.as_str()).await.ok();
        Ok(())
    }
}

struct MockPortal {
    conn: Connection,
    state: Shared,
}

impl MockPortal {
    async fn start(bus: &Bus, version: u32, trigger: &str) -> Self {
        let state: Shared = Arc::new(Mutex::new(State {
            version,
            trigger: trigger.into(),
            ..State::default()
        }));
        let conn = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name("org.freedesktop.portal.Desktop")
            .unwrap()
            .serve_at(DESKTOP_PATH, Registry(state.clone()))
            .unwrap()
            .serve_at(DESKTOP_PATH, Shortcuts(state.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        Self { conn, state }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn session(&self) -> String {
        self.state()
            .sessions
            .last()
            .cloned()
            .expect("a session was created")
    }

    async fn activate(&self, client: &Connection, id: &str, token: Option<&str>) {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        if let Some(token) = token {
            options.insert("activation_token", Value::from(token));
        }
        let session = ObjectPath::try_from(self.session()).unwrap();
        self.conn
            .emit_signal(
                client.unique_name().map(|n| n.as_ref()),
                DESKTOP_PATH,
                GS_IFACE,
                "Activated",
                &(session, id, 1_700_000_000_000u64, options),
            )
            .await
            .unwrap();
    }

    async fn change_trigger(&self, client: &Connection, trigger: &str) {
        let session = ObjectPath::try_from(self.session()).unwrap();
        let info: HashMap<&str, Value<'_>> = HashMap::from([
            ("description", Value::from("Translate the selected text")),
            ("trigger_description", Value::from(trigger)),
        ]);
        self.conn
            .emit_signal(
                client.unique_name().map(|n| n.as_ref()),
                DESKTOP_PATH,
                GS_IFACE,
                "ShortcutsChanged",
                &(session, vec![(SHORTCUT_ID, info)]),
            )
            .await
            .unwrap();
    }
}

async fn within<F: std::future::Future>(fut: F) -> F::Output {
    tokio::time::timeout(STEP, fut)
        .await
        .expect("step timed out")
}

fn caps(version: Option<u32>, compositor: Compositor) -> Capabilities {
    Capabilities {
        portal_global_shortcuts: version,
        compositor,
        ..Capabilities::default()
    }
}

fn command() -> ToggleCommand {
    ToggleCommand::new("/opt/QuickTranslate.AppImage")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_bind_activate_change_configure_close() {
    let bus = Bus::spawn();
    let mock = MockPortal::start(&bus, 2, "Ctrl+Alt+C").await;
    let client = bus.connect().await;

    let probe = bus.connect().await;
    assert_eq!(within(portal::detect_version(&probe)).await, Some(2));
    mock.state().calls.clear();

    let mechanism = within(shortcut::start(
        Some(&client),
        &caps(Some(2), Compositor::Kde),
        &command(),
    ))
    .await;
    assert_eq!(mechanism.label(), "System shortcut (portal)");
    let Mechanism::Portal(mut portal) = mechanism else {
        panic!("expected the portal mechanism");
    };

    {
        let state = mock.state();
        let me = client.unique_name().unwrap().to_string();
        assert_eq!(
            state.calls.first(),
            Some(&format!("{me} Register")),
            "{:?}",
            state.calls
        );
        assert_eq!(state.registered, [APP_ID]);
        let order: Vec<&str> = state
            .calls
            .iter()
            .filter_map(|c| c.split_once(' ').map(|(_, m)| m))
            .filter(|m| !m.starts_with("Get"))
            .collect();
        assert_eq!(
            order,
            [
                "Register",
                "CreateSession",
                "BindShortcuts",
                "ListShortcuts"
            ]
        );
        let (id, description, preferred) = &state.bound[0];
        assert_eq!(id, SHORTCUT_ID);
        assert!(!description.is_empty());
        assert_eq!(preferred.as_deref(), Some(PREFERRED_TRIGGER));
        assert_eq!(PREFERRED_TRIGGER, "CTRL+ALT+c");
    }
    assert_eq!(portal.version(), 2);
    assert!(portal.can_configure());
    assert_eq!(portal.current_trigger().as_deref(), Some("Ctrl+Alt+C"));

    let mut activations = portal.take_activations().expect("first take");
    assert!(portal.take_activations().is_none());
    mock.activate(&client, SHORTCUT_ID, Some("token-123")).await;
    assert_eq!(
        within(activations.next()).await,
        Some(Activation {
            token: Some("token-123".into())
        })
    );
    mock.activate(&client, "someone-else", Some("ignored"))
        .await;
    mock.activate(&client, SHORTCUT_ID, None).await;
    assert_eq!(
        within(activations.next()).await,
        Some(Activation { token: None })
    );

    let mut changes = Box::pin(portal.trigger_changes());
    mock.change_trigger(&client, "Meta+T").await;
    assert_eq!(within(changes.next()).await, Some(Some("Meta+T".into())));
    assert_eq!(portal.current_trigger().as_deref(), Some("Meta+T"));

    within(portal.configure(Some("cfg-token".into())))
        .await
        .unwrap();
    assert_eq!(
        mock.state().configure_tokens,
        [Some("cfg-token".to_owned())]
    );

    let session = mock.session();
    within(portal.close()).await.unwrap();
    assert_eq!(mock.state().closed, [session]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v1_offers_no_configure() {
    let bus = Bus::spawn();
    let mock = MockPortal::start(&bus, 1, "Ctrl+Alt+C").await;
    let client = bus.connect().await;

    let portal = within(PortalShortcuts::start(&client)).await.unwrap();
    assert_eq!(portal.version(), 1);
    assert!(!portal.can_configure());
    let err = within(portal.configure(None)).await.unwrap_err();
    assert!(
        matches!(err, ConfigureError::Unsupported { version: 1 }),
        "{err}"
    );
    assert!(err.to_string().contains("system shortcut settings"));
    assert!(
        !mock
            .state()
            .calls
            .iter()
            .any(|c| c.ends_with("ConfigureShortcuts"))
    );

    // v1 still delivers activations, without a token.
    let mut portal = portal;
    let mut activations = portal.take_activations().unwrap();
    mock.activate(&client, SHORTCUT_ID, None).await;
    assert_eq!(
        within(activations.next()).await,
        Some(Activation { token: None })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binds_on_every_start_and_keeps_persisted_trigger() {
    let bus = Bus::spawn();
    let mock = MockPortal::start(&bus, 2, "Meta+T").await;

    for _ in 0..2 {
        let client = bus.connect().await;
        let portal = within(PortalShortcuts::start(&client)).await.unwrap();
        assert_eq!(portal.current_trigger().as_deref(), Some("Meta+T"));
        within(portal.close()).await.unwrap();
    }
    let state = mock.state();
    assert_eq!(state.bound.len(), 2);
    assert!(state.bound.iter().all(|(id, ..)| id == SHORTCUT_ID));
    assert_eq!(state.registered.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declined_bind_keeps_portal_without_trigger() {
    let bus = Bus::spawn();
    let mock = MockPortal::start(&bus, 2, "Ctrl+Alt+C").await;
    mock.state().cancel_bind = true;
    let client = bus.connect().await;

    let portal = within(PortalShortcuts::start(&client)).await.unwrap();
    assert_eq!(portal.current_trigger(), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registry_failure_falls_back_to_compositor() {
    let bus = Bus::spawn();
    let mock = MockPortal::start(&bus, 2, "Ctrl+Alt+C").await;
    mock.state().registry_fails = true;
    let client = bus.connect().await;

    let err = within(PortalShortcuts::start(&client)).await.unwrap_err();
    assert!(err.is_registry(), "{err}");

    let mechanism = within(shortcut::start(
        Some(&client),
        &caps(Some(2), Compositor::Niri),
        &command(),
    ))
    .await;
    let Mechanism::Compositor(snippet) = mechanism else {
        panic!("expected the compositor fallback");
    };
    assert_eq!(snippet.format, SnippetFormat::Niri);
    assert!(
        snippet
            .text
            .contains("\"/opt/QuickTranslate.AppImage\" \"--toggle\"")
    );
    assert!(
        !mock
            .state()
            .calls
            .iter()
            .any(|c| c.ends_with("CreateSession"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_portal_falls_back_to_compositor() {
    let bus = Bus::spawn();
    let client = bus.connect().await;

    assert_eq!(within(portal::detect_version(&client)).await, None);
    // Even with a stale capability, the failed Registry call yields the fallback.
    let mechanism = within(shortcut::start(
        Some(&client),
        &caps(Some(1), Compositor::Sway),
        &command(),
    ))
    .await;
    assert!(
        matches!(&mechanism, Mechanism::Compositor(s) if s.format == SnippetFormat::Sway),
        "{mechanism:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_capability_skips_the_portal() {
    let bus = Bus::spawn();
    let mock = MockPortal::start(&bus, 2, "Ctrl+Alt+C").await;
    let client = bus.connect().await;

    let mechanism = within(shortcut::start(
        Some(&client),
        &caps(None, Compositor::Hyprland),
        &command(),
    ))
    .await;
    assert!(matches!(&mechanism, Mechanism::Compositor(s) if s.format == SnippetFormat::Hyprland));
    assert!(mock.state().calls.is_empty());

    let mechanism = within(shortcut::start(
        None,
        &caps(Some(2), Compositor::Unknown),
        &command(),
    ))
    .await;
    assert!(matches!(&mechanism, Mechanism::Compositor(s) if s.format == SnippetFormat::Generic));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interface_missing_is_unavailable() {
    let bus = Bus::spawn();
    let state: Shared = Arc::new(Mutex::new(State::default()));
    let _portal = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.freedesktop.portal.Desktop")
        .unwrap()
        .serve_at(DESKTOP_PATH, Registry(state.clone()))
        .unwrap()
        .build()
        .await
        .unwrap();
    let client = bus.connect().await;

    assert_eq!(within(portal::detect_version(&client)).await, None);
    let err = within(PortalShortcuts::start(&client)).await.unwrap_err();
    assert!(matches!(err, shortcut::PortalError::Unavailable), "{err}");
    assert_eq!(state.lock().unwrap().registered, [APP_ID]);
}

#[test]
fn handles_are_send() {
    fn assert_send<T: Send + 'static>() {}
    assert_send::<PortalShortcuts>();
    assert_send::<Mechanism>();
    assert_send::<portal::ActivationStream>();
}
