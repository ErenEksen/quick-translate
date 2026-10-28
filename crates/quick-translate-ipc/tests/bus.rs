//! Client calls reach a mock daemon on a private `dbus-daemon --session` (task 4.1), and the
//! socket fallback carries the same commands.

use std::collections::HashMap;
use std::time::Duration;

use quick_translate_ipc::client::Request;
use quick_translate_ipc::proxy::QuickTranslateProxy;
use quick_translate_ipc::socket::{self, SocketClaimOutcome};
use quick_translate_ipc::testing::PrivateBus;
use quick_translate_ipc::{
    BusClaim, DaemonCommand, DaemonStatus, Endpoints, ForwardError, Options, Reply, claim_bus_name,
    forward,
};
use tokio::sync::mpsc;
use zbus::zvariant::Value;

fn status() -> DaemonStatus {
    DaemonStatus {
        version: "9.9.9".into(),
        pid: 4242,
        capabilities: "layer-shell: v4\nportal: v2".into(),
        popup_visible: true,
    }
}

async fn connect(address: &str) -> zbus::Connection {
    zbus::connection::Builder::address(address)
        .expect("address")
        .build()
        .await
        .expect("connect")
}

/// Runs a blocking forward while the "daemon" answers the one command it produces.
async fn roundtrip(
    endpoints: &Endpoints,
    request: Request,
    commands: &mut mpsc::Receiver<DaemonCommand>,
) -> (Result<Reply, ForwardError>, DaemonCommand) {
    let endpoints = endpoints.clone();
    let call = tokio::task::spawn_blocking(move || forward(&request, &endpoints));
    let mut command = tokio::time::timeout(Duration::from_secs(5), commands.recv())
        .await
        .expect("command arrives")
        .expect("channel open");
    if let DaemonCommand::Status { reply } = command {
        reply.send(status()).expect("reply");
        command = DaemonCommand::Status {
            reply: tokio::sync::oneshot::channel().0,
        };
    }
    (call.await.expect("join"), command)
}

fn token(value: &str) -> Options {
    Options {
        activation_token: Some(value.into()),
    }
}

async fn check_all_commands(endpoints: &Endpoints, rx: &mut mpsc::Receiver<DaemonCommand>) {
    let (reply, command) = roundtrip(endpoints, Request::Toggle(token("tok-1")), rx).await;
    assert_eq!(reply.expect("toggle"), Reply::Done);
    assert!(
        matches!(&command, DaemonCommand::Toggle { options } if *options == token("tok-1")),
        "{command:?}"
    );

    let (reply, command) = roundtrip(endpoints, Request::Show(Options::default()), rx).await;
    assert_eq!(reply.expect("show"), Reply::Done);
    assert!(
        matches!(&command, DaemonCommand::Show { options } if options.activation_token.is_none()),
        "{command:?}"
    );

    let (reply, command) = roundtrip(endpoints, Request::Settings, rx).await;
    assert_eq!(reply.expect("settings"), Reply::Done);
    assert!(matches!(command, DaemonCommand::Settings));

    let (reply, command) = roundtrip(endpoints, Request::Status, rx).await;
    assert_eq!(reply.expect("status"), Reply::Status(status()));
    assert!(matches!(command, DaemonCommand::Status { .. }));

    let (reply, command) = roundtrip(endpoints, Request::Quit, rx).await;
    assert_eq!(reply.expect("quit"), Reply::Done);
    assert!(matches!(command, DaemonCommand::Quit));
}

#[tokio::test(flavor = "multi_thread")]
async fn blocking_client_reaches_mock_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bus = PrivateBus::start(dir.path()).expect("dbus-daemon");
    let endpoints = Endpoints {
        bus_address: Some(bus.address().to_owned()),
        socket: None,
        timeout: Duration::from_secs(5),
    };

    let before = tokio::task::spawn_blocking({
        let endpoints = endpoints.clone();
        move || forward(&Request::Status, &endpoints)
    })
    .await
    .expect("join");
    assert!(
        matches!(before, Err(ForwardError::NotRunning)),
        "{before:?}"
    );

    let server = connect(bus.address()).await;
    let (tx, mut rx) = mpsc::channel(8);
    assert_eq!(
        claim_bus_name(&server, tx).await.expect("claim"),
        BusClaim::Owner
    );

    check_all_commands(&endpoints, &mut rx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn async_proxy_reaches_mock_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bus = PrivateBus::start(dir.path()).expect("dbus-daemon");
    let server = connect(bus.address()).await;
    let (tx, mut rx) = mpsc::channel(8);
    assert_eq!(
        claim_bus_name(&server, tx).await.expect("claim"),
        BusClaim::Owner
    );

    let client = connect(bus.address()).await;
    let proxy = QuickTranslateProxy::new(&client).await.expect("proxy");

    let options = HashMap::from([("activation-token", Value::from("xdg-77"))]);
    proxy.toggle(options).await.expect("toggle");
    let command = rx.recv().await.expect("command");
    assert!(
        matches!(&command, DaemonCommand::Toggle { options } if *options == token("xdg-77")),
        "{command:?}"
    );

    let answer = tokio::spawn(async move {
        if let Some(DaemonCommand::Status { reply }) = rx.recv().await {
            reply.send(status()).expect("reply");
        }
    });
    let dict = proxy.status().await.expect("status");
    answer.await.expect("join");
    assert_eq!(DaemonStatus::from_dict(&dict), status());
}

#[tokio::test(flavor = "multi_thread")]
async fn second_claim_loses_with_do_not_queue() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bus = PrivateBus::start(dir.path()).expect("dbus-daemon");
    let first = connect(bus.address()).await;
    let second = connect(bus.address()).await;
    let (tx, _rx) = mpsc::channel(8);

    let (a, b) = tokio::join!(
        claim_bus_name(&first, tx.clone()),
        claim_bus_name(&second, tx.clone())
    );
    let mut claims = [a.expect("first"), b.expect("second")];
    claims.sort_by_key(|claim| *claim == BusClaim::AlreadyRunning);
    assert_eq!(claims, [BusClaim::Owner, BusClaim::AlreadyRunning]);
}

#[tokio::test(flavor = "multi_thread")]
async fn socket_fallback_carries_all_commands() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("quick-translate.sock");
    let claim = match socket::claim(&path).expect("claim") {
        SocketClaimOutcome::Claimed(claim) => claim,
        SocketClaimOutcome::AlreadyRunning => panic!("lock held"),
    };
    let (tx, mut rx) = mpsc::channel(8);
    let server = tokio::spawn(socket::serve(claim.tokio_listener().expect("listener"), tx));

    let endpoints = Endpoints {
        bus_address: Some(format!("unix:path={}", dir.path().join("no-bus").display())),
        socket: Some(path.clone()),
        timeout: Duration::from_secs(5),
    };
    check_all_commands(&endpoints, &mut rx).await;

    drop(rx);
    server.abort();
    drop(claim);
    let after = tokio::task::spawn_blocking(move || forward(&Request::Settings, &endpoints))
        .await
        .expect("join");
    assert!(matches!(after, Err(ForwardError::NotRunning)), "{after:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_methods_and_signals_reach_mock_server() {
    use futures::StreamExt as _;
    use quick_translate_core::BackendKind;
    use quick_translate_ipc::proxy::error_message;
    use quick_translate_ipc::{
        ShortcutInfo, ShortcutMechanism, emit_settings_requested, emit_shortcut_changed,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let bus = PrivateBus::start(dir.path()).expect("dbus-daemon");
    let server = connect(bus.address()).await;
    let (tx, mut rx) = mpsc::channel(8);
    assert_eq!(
        claim_bus_name(&server, tx).await.expect("claim"),
        BusClaim::Owner
    );
    let info = ShortcutInfo {
        mechanism: ShortcutMechanism::Portal,
        label: "System shortcut (portal)".into(),
        portal_version: 2,
        trigger: Some("Ctrl+Alt+C".into()),
        can_configure: true,
        ..ShortcutInfo::default()
    };
    let daemon = tokio::spawn({
        let info = info.clone();
        async move {
            let mut seen = Vec::new();
            while let Some(command) = rx.recv().await {
                match command {
                    DaemonCommand::ReloadConfig { reply } => {
                        reply.send(Ok(())).expect("reply");
                        seen.push("reload".to_owned());
                    }
                    DaemonCommand::SetSessionSecret { backend, key } => {
                        assert_eq!(format!("{key:?}"), "<redacted>");
                        seen.push(format!("secret {backend:?} {}", key.expose()));
                    }
                    DaemonCommand::ShortcutInfo { reply } => {
                        reply.send(info.clone()).expect("reply");
                    }
                    DaemonCommand::ConfigureShortcut {
                        activation_token,
                        reply,
                    } => {
                        seen.push(format!("configure {activation_token:?}"));
                        reply.send(Ok(())).expect("reply");
                    }
                    DaemonCommand::SetEvdevEnabled { enabled, reply } => {
                        seen.push(format!("evdev {enabled}"));
                        reply
                            .send(if enabled {
                                Err("no permission to read /dev/input".into())
                            } else {
                                Ok(())
                            })
                            .expect("reply");
                    }
                    DaemonCommand::Quit => break,
                    other => panic!("unexpected {other:?}"),
                }
            }
            seen
        }
    });

    let client = connect(bus.address()).await;
    let proxy = QuickTranslateProxy::new(&client).await.expect("proxy");
    let mut changes = proxy.receive_shortcut_changed().await.expect("subscribe");
    let mut raises = proxy.receive_settings_requested().await.expect("subscribe");

    proxy.reload_config().await.expect("reload");
    proxy
        .set_session_secret("deepl", "key:fx")
        .await
        .expect("session secret");
    let err = proxy
        .set_session_secret("google", "x")
        .await
        .expect_err("unknown backend");
    assert!(error_message(&err).contains("google"), "{err}");
    let dict = proxy.shortcut_info().await.expect("info");
    assert_eq!(ShortcutInfo::from_dict(&dict), info);
    proxy.configure_shortcut("").await.expect("configure");
    proxy.configure_shortcut("tok").await.expect("configure");
    proxy.set_evdev_enabled(false).await.expect("disable");
    let err = proxy.set_evdev_enabled(true).await.expect_err("denied");
    assert_eq!(error_message(&err), "no permission to read /dev/input");

    emit_shortcut_changed(&server, Some("Super+T"))
        .await
        .expect("emit");
    let signal = tokio::time::timeout(Duration::from_secs(5), changes.next())
        .await
        .expect("signal")
        .expect("stream");
    assert_eq!(signal.args().expect("args").trigger(), &"Super+T");
    emit_settings_requested(&server).await.expect("emit");
    tokio::time::timeout(Duration::from_secs(5), raises.next())
        .await
        .expect("signal")
        .expect("stream");

    proxy.quit().await.expect("quit");
    let seen = daemon.await.expect("join");
    assert_eq!(
        seen,
        [
            "reload".to_owned(),
            format!("secret {:?} key:fx", BackendKind::Deepl),
            "configure None".into(),
            "configure Some(\"tok\")".into(),
            "evdev false".into(),
            "evdev true".into(),
        ]
    );
}
