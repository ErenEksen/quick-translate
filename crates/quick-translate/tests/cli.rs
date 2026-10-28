//! End-to-end CLI tests. Every process runs with a cleared environment pointing at a private
//! `dbus-daemon`, a temp `XDG_RUNTIME_DIR` and a fake Wayland socket, so nothing touches the
//! user's session.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use quick_translate_ipc::client::Request;
use quick_translate_ipc::testing::PrivateBus;
use quick_translate_ipc::{
    BusClaim, DaemonCommand, DaemonStatus, Endpoints, ForwardError, Reply, claim_bus_name, forward,
};

const BIN: &str = env!("CARGO_BIN_EXE_quick-translate");

struct TestEnv {
    dir: tempfile::TempDir,
    bus: Option<PrivateBus>,
    _wayland: UnixListener,
}

impl TestEnv {
    /// With a private session bus.
    fn new() -> Self {
        let mut env = Self::without_bus();
        env.bus = Some(PrivateBus::start(env.dir.path()).expect("dbus-daemon"));
        env
    }

    /// No session bus reachable: the socket fallback is used.
    fn without_bus() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        for sub in ["run", "home", "data", "config"] {
            fs::create_dir(dir.path().join(sub)).expect("mkdir");
        }
        fs::set_permissions(dir.path().join("run"), fs::Permissions::from_mode(0o700))
            .expect("chmod");
        let wayland = UnixListener::bind(dir.path().join("run/wayland-test")).expect("bind");
        Self {
            dir,
            bus: None,
            _wayland: wayland,
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn bus_address(&self) -> String {
        match &self.bus {
            Some(bus) => bus.address().to_owned(),
            None => format!("unix:path={}", self.path("no-bus").display()),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        command
            .args(args)
            .env_clear()
            .env("HOME", self.path("home"))
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("WAYLAND_DISPLAY", "wayland-test")
            .env("DBUS_SESSION_BUS_ADDRESS", self.bus_address())
            // The fake Wayland socket never answers: run the daemon without popup surfaces.
            .env("QT_HEADLESS", "1")
            .stdin(Stdio::null());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run")
    }

    fn endpoints(&self) -> Endpoints {
        Endpoints {
            bus_address: Some(self.bus_address()),
            socket: Some(self.path("run/quick-translate.sock")),
            timeout: Duration::from_secs(3),
        }
    }

    fn try_status(&self) -> Result<Option<DaemonStatus>, ForwardError> {
        match forward(&Request::Status, &self.endpoints()) {
            Ok(Reply::Status(status)) => Ok(Some(status)),
            Ok(Reply::Done) => Err(ForwardError::Protocol("empty status reply".into())),
            Err(ForwardError::NotRunning) => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn status(&self) -> Option<DaemonStatus> {
        self.try_status().expect("status")
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let status = match self.try_status() {
            Ok(status) => status,
            Err(err) => {
                eprintln!("cleanup: status failed: {err}");
                None
            }
        };
        if let Some(status) = status {
            let _ = forward(&Request::Quit, &self.endpoints());
            let deadline = Instant::now() + Duration::from_secs(1);
            while matches!(self.try_status(), Ok(Some(_))) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if matches!(self.try_status(), Ok(Some(_))) {
                let _ = Command::new("kill")
                    .arg(status.pid.to_string())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status {:?}\nstdout: {}\nstderr: {}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

fn wait_exit(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

#[test]
fn version_works_without_any_session() {
    let output = Command::new(BIN)
        .arg("--version")
        .env_clear()
        .output()
        .expect("run");
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("quick-translate {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_prints_usage() {
    let output = Command::new(BIN)
        .arg("--help")
        .env_clear()
        .output()
        .expect("run");
    assert_success(&output);
    for flag in [
        "--toggle",
        "--show",
        "--settings",
        "--quit",
        "--daemon",
        "--remove-integration",
        "--status",
        "--version",
    ] {
        assert!(stdout(&output).contains(flag), "{flag} missing from usage");
    }
}

#[test]
fn unknown_argument_prints_usage_without_contacting_the_daemon() {
    let env = TestEnv::without_bus();
    // Listeners standing in for the bus and the socket record any connection attempt.
    let bus = UnixListener::bind(env.path("no-bus")).expect("bind bus");
    let sock = UnixListener::bind(env.path("run/quick-translate.sock")).expect("bind sock");
    bus.set_nonblocking(true).expect("nonblocking");
    sock.set_nonblocking(true).expect("nonblocking");

    for args in [&["--bogus"][..], &["--toggle", "--show"], &["toggle"]] {
        let output = env.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            stderr(&output).contains("Usage: quick-translate"),
            "{args:?}"
        );
    }
    assert!(bus.accept().is_err(), "the bus was contacted");
    assert!(sock.accept().is_err(), "the socket was contacted");
}

#[test]
fn wayland_display_unset_is_refused() {
    let env = TestEnv::new();
    for args in [
        &[][..],
        &["--toggle"],
        &["--show"],
        &["--settings"],
        &["--status"],
        &["--daemon"],
    ] {
        let output = env
            .command(args)
            .env_remove("WAYLAND_DISPLAY")
            .output()
            .expect("run");
        assert!(!output.status.success(), "{args:?} succeeded");
        assert!(
            stderr(&output).contains("requires a Wayland session"),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    assert_eq!(env.status(), None, "a daemon was started");
}

#[test]
fn unreachable_wayland_socket_is_named() {
    let env = TestEnv::new();
    let output = env
        .command(&["--daemon"])
        .env("WAYLAND_DISPLAY", "wayland-missing")
        .output()
        .expect("run");
    assert!(!output.status.success());
    let expected = env.path("run/wayland-missing");
    assert!(
        stderr(&output).contains(&expected.display().to_string()),
        "{}",
        stderr(&output)
    );

    // The same check runs before `--toggle` spawns a daemon.
    let output = env
        .command(&["--toggle"])
        .env("WAYLAND_DISPLAY", "/nonexistent/wayland-0")
        .output()
        .expect("run");
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("/nonexistent/wayland-0"),
        "{}",
        stderr(&output)
    );
    assert_eq!(env.status(), None);
}

fn lifecycle(env: &TestEnv) {
    assert!(stdout(&env.run(&["--status"])).contains("daemon: not running"));

    // Toggle with no daemon running starts one, then shows the popup.
    assert_success(&env.run(&["--toggle"]));
    let status = env.status().expect("daemon started");
    assert!(status.popup_visible);
    let report = env.run(&["--status"]);
    assert_success(&report);
    assert!(
        stdout(&report).contains("daemon: running"),
        "{}",
        stdout(&report)
    );
    assert!(stdout(&report).contains("popup: visible"));

    // Toggle while visible hides it; the daemon keeps running.
    assert_success(&env.run(&["--toggle"]));
    let after = env.status().expect("still running");
    assert!(!after.popup_visible);
    assert_eq!(after.pid, status.pid);

    assert_success(&env.run(&["--show"]));
    assert!(env.status().expect("running").popup_visible);
    assert_success(&env.run(&["--settings"]));
    assert_success(&env.run(&[]));
    assert_eq!(
        env.status().expect("running").pid,
        status.pid,
        "second daemon started"
    );

    assert_success(&env.run(&["--quit"]));
    assert_eq!(env.status(), None, "daemon still answers after --quit");
    assert_success(&env.run(&["--quit"]));
}

#[test]
fn lifecycle_on_the_session_bus() {
    lifecycle(&TestEnv::new());
}

#[test]
fn lifecycle_on_the_socket_fallback() {
    let env = TestEnv::without_bus();
    lifecycle(&env);
    assert!(!env.path("run/quick-translate.sock").exists());
}

#[test]
fn no_arguments_start_the_daemon() {
    let env = TestEnv::new();
    assert_success(&env.run(&[]));
    assert!(env.status().is_some());
}

#[test]
fn settings_and_show_start_the_daemon() {
    let env = TestEnv::new();
    assert_success(&env.run(&["--show"]));
    assert!(env.status().expect("started").popup_visible);
    assert_success(&env.run(&["--quit"]));
    assert_success(&env.run(&["--settings"]));
    assert!(env.status().is_some());
}

#[test]
fn forwarding_is_fast() {
    let env = TestEnv::new();
    assert_success(&env.run(&[]));
    assert!(env.status().is_some());
    let mut times: Vec<Duration> = (0..7)
        .map(|_| {
            let start = Instant::now();
            assert_success(&env.run(&["--settings"]));
            start.elapsed()
        })
        .collect();
    times.sort();
    let median = times[times.len() / 2];
    assert!(
        median < Duration::from_millis(200),
        "median forward {median:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn activation_token_reaches_the_daemon() {
    let env = TestEnv::new();
    let conn = zbus::connection::Builder::address(env.bus_address().as_str())
        .expect("address")
        .build()
        .await
        .expect("connect");
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    assert_eq!(
        claim_bus_name(&conn, tx).await.expect("claim"),
        BusClaim::Owner
    );

    let mut command = env.command(&["--toggle"]);
    command.env("XDG_ACTIVATION_TOKEN", "token-from-launcher");
    let output = tokio::task::spawn_blocking(move || command.output().expect("run"))
        .await
        .expect("join");
    assert_success(&output);
    match rx.recv().await.expect("command") {
        DaemonCommand::Toggle { options } => {
            assert_eq!(
                options.activation_token.as_deref(),
                Some("token-from-launcher")
            );
        }
        other => panic!("unexpected {other:?}"),
    }

    let mut command = env.command(&["--show"]);
    let output = tokio::task::spawn_blocking(move || command.output().expect("run"))
        .await
        .expect("join");
    assert_success(&output);
    match rx.recv().await.expect("command") {
        DaemonCommand::Show { options } => assert_eq!(options.activation_token, None),
        other => panic!("unexpected {other:?}"),
    }
}

fn race(env: &TestEnv) {
    let mut a = env
        .command(&["--daemon"])
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a");
    let mut b = env
        .command(&["--daemon"])
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn b");
    let (a_pid, b_pid) = (a.id(), b.id());

    let deadline = Instant::now() + Duration::from_secs(3);
    let (mut a_exit, mut b_exit) = (None, None);
    while a_exit.is_none() && b_exit.is_none() && Instant::now() < deadline {
        a_exit = a.try_wait().expect("try_wait");
        b_exit = b.try_wait().expect("try_wait");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Give the other one time to (wrongly) exit too.
    std::thread::sleep(Duration::from_millis(300));
    let a_exit = a_exit.or_else(|| a.try_wait().expect("try_wait"));
    let b_exit = b_exit.or_else(|| b.try_wait().expect("try_wait"));
    let (loser, survivor_pid, survivor) = match (a_exit, b_exit) {
        (Some(status), None) => (status, b_pid, &mut b),
        (None, Some(status)) => (status, a_pid, &mut a),
        other => {
            let _ = a.kill();
            let _ = b.kill();
            panic!("expected exactly one survivor, got {other:?}");
        }
    };
    assert!(loser.success(), "losing daemon exited with {loser:?}");
    assert_eq!(env.status().expect("survivor answers").pid, survivor_pid);

    assert_success(&env.run(&["--quit"]));
    let status = wait_exit(survivor, Duration::from_secs(3)).expect("survivor exits on quit");
    assert!(status.success(), "{status:?}");
}

#[test]
fn two_daemons_started_together_leave_exactly_one() {
    let env = TestEnv::new();
    for _ in 0..5 {
        race(&env);
    }
}

#[test]
fn two_socket_daemons_started_together_leave_exactly_one() {
    let env = TestEnv::without_bus();
    for _ in 0..5 {
        race(&env);
    }
}

#[test]
fn daemon_exits_when_the_bus_goes_away() {
    let mut env = TestEnv::new();
    let mut daemon = env
        .command(&["--daemon"])
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let deadline = Instant::now() + Duration::from_secs(3);
    while env.status().is_none() {
        assert!(Instant::now() < deadline, "daemon did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    env.bus = None;
    let status = wait_exit(&mut daemon, Duration::from_secs(3)).expect("daemon exits");
    assert!(status.success(), "{status:?}");
}

fn touch(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, content).expect("write");
}

#[test]
fn remove_integration_stops_the_daemon_and_keeps_config() {
    let env = TestEnv::new();
    let app = "io.github.quicktranslate.QuickTranslate";
    let files = [
        env.path(&format!("data/applications/{app}.desktop")),
        env.path(&format!("data/icons/hicolor/scalable/apps/{app}.svg")),
        env.path(&format!("config/autostart/{app}.desktop")),
    ];
    for file in &files {
        touch(file, "x");
    }
    let config = env.path("config/quick-translate/config.toml");
    touch(&config, "backend = \"deepl\"\n");

    assert_success(&env.run(&[]));
    assert!(env.status().is_some());

    let output = env.run(&["--remove-integration"]);
    assert_success(&output);
    for file in &files {
        assert!(!file.exists(), "{} still exists", file.display());
    }
    assert!(config.exists());
    assert_eq!(env.status(), None, "daemon still running");

    // Idempotent, and works without a Wayland session.
    let output = env
        .command(&["--remove-integration"])
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .expect("run");
    assert_success(&output);
}

/// Live `--settings-window` children of `parent`.
fn settings_windows(parent: u32) -> Vec<u32> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").expect("proc").flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let cmdline = fs::read(entry.path().join("cmdline")).unwrap_or_default();
        if !cmdline
            .split(|b| *b == 0)
            .any(|arg| arg == b"--settings-window")
        {
            continue;
        }
        let status = fs::read_to_string(entry.path().join("status")).unwrap_or_default();
        let ppid = status
            .lines()
            .find_map(|l| l.strip_prefix("PPid:"))
            .and_then(|v| v.trim().parse::<u32>().ok());
        if ppid == Some(parent) {
            found.push(pid);
        }
    }
    found
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn settings_window_is_single_instance_and_closes_with_the_daemon() {
    let env = TestEnv::new();
    assert_success(&env.run(&["--settings"]));
    let daemon = env.status().expect("daemon started").pid;
    wait_for("the settings window", || {
        settings_windows(daemon).len() == 1
    });
    let window = settings_windows(daemon);

    assert_success(&env.run(&["--settings"]));
    assert_success(&env.run(&[]));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        settings_windows(daemon),
        window,
        "a second window was spawned"
    );

    assert_success(&env.run(&["--quit"]));
    wait_for("the settings window to close", || {
        !Path::new(&format!("/proc/{}/cmdline", window[0])).exists()
            || fs::read(format!("/proc/{}/cmdline", window[0]))
                .unwrap_or_default()
                .is_empty()
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_answers_settings_calls() {
    use quick_translate_ipc::ShortcutMechanism;
    use quick_translate_ipc::proxy::{QuickTranslateProxy, error_message};

    let env = TestEnv::new();
    let start = env.command(&["--daemon"]).stderr(Stdio::null()).spawn();
    let mut daemon = start.expect("spawn");
    let address = env.bus_address();
    tokio::task::spawn_blocking({
        let endpoints = env.endpoints();
        move || {
            wait_for("the daemon", || {
                matches!(forward(&Request::Status, &endpoints), Ok(Reply::Status(_)))
            })
        }
    })
    .await
    .expect("join");

    let conn = zbus::connection::Builder::address(address.as_str())
        .expect("address")
        .build()
        .await
        .expect("connect");
    let proxy = QuickTranslateProxy::new(&conn).await.expect("proxy");

    let info = quick_translate_ipc::ShortcutInfo::from_dict(
        &proxy.shortcut_info().await.expect("shortcut info"),
    );
    assert_eq!(info.mechanism, ShortcutMechanism::Compositor);
    let snippet = info.snippet.expect("snippet");
    assert!(snippet.text.contains("--toggle"), "{}", snippet.text);
    assert!(!info.evdev_active);
    assert!(info.evdev_warning.contains("everything you type"));
    let err = proxy.configure_shortcut("").await.expect_err("no portal");
    assert!(error_message(&err).contains("compositor"), "{err}");
    proxy
        .set_session_secret("deepl", "session-key")
        .await
        .expect("session secret");
    proxy.set_session_secret("deepl", "").await.expect("clear");
    proxy.set_evdev_enabled(false).await.expect("evdev off");

    // ReloadConfig re-runs the desktop integration with the saved autostart flag.
    let autostart = env.path("config/autostart/io.github.quicktranslate.QuickTranslate.desktop");
    let config = env.path("config/quick-translate/config.toml");
    touch(&config, "autostart = false\n");
    proxy.reload_config().await.expect("reload");
    assert!(
        env.path("data/applications/io.github.quicktranslate.QuickTranslate.desktop")
            .exists()
    );
    assert!(!autostart.exists());
    touch(&config, "autostart = true\n");
    proxy.reload_config().await.expect("reload");
    assert!(autostart.exists());
    touch(&config, "autostart = [\n");
    let err = proxy.reload_config().await.expect_err("malformed");
    assert!(error_message(&err).contains("malformed"), "{err}");

    proxy.quit().await.ok();
    let status =
        tokio::task::spawn_blocking(move || wait_exit(&mut daemon, Duration::from_secs(3)))
            .await
            .expect("join");
    assert!(status.is_some_and(|s| s.success()));
}
