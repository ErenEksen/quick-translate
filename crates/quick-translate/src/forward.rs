//! CLI side: forward a command to the running daemon, starting one when needed.
//! Nothing on the forwarding path creates a Tokio runtime, a GUI or an extra thread; only
//! `--status` without a running daemon probes the capabilities itself.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command as Process, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use quick_translate_ipc::client::Request;
use quick_translate_ipc::{Endpoints, ForwardError, Options, Reply, forward};
use quick_translate_platform::integration;
use quick_translate_platform::shortcut::portal;

use crate::cli::Command;
use crate::gate;

const START_TIMEOUT: Duration = Duration::from_secs(3);
const STOP_TIMEOUT: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("quick-translate: {message}");
    ExitCode::FAILURE
}

/// No arguments, `--toggle`, `--show` and `--settings`.
pub fn activate(command: Command) -> ExitCode {
    let request = match command {
        Command::Toggle => Request::Toggle(Options::from_env()),
        Command::Show => Request::Show(Options::from_env()),
        _ => Request::Settings,
    };
    let endpoints = Endpoints::from_env();
    match forward(&request, &endpoints) {
        Ok(_) => return ExitCode::SUCCESS,
        Err(ForwardError::NotRunning) => {}
        Err(err) => return fail(err),
    }
    if let Err(err) = gate::check_connection() {
        return fail(err);
    }
    if let Err(err) = start_daemon(&endpoints) {
        return fail(err);
    }
    if command == Command::Default {
        return ExitCode::SUCCESS;
    }
    match forward(&request, &endpoints) {
        Ok(_) => ExitCode::SUCCESS,
        Err(err) => fail(err),
    }
}

/// Spawns `<exe> --daemon` detached and waits until it answers.
fn start_daemon(endpoints: &Endpoints) -> io::Result<()> {
    use std::os::unix::process::CommandExt;

    // Inside an AppImage `current_exe()` lives in a mount that vanishes when this process exits.
    let exe: PathBuf = integration::exec_path_from_env()?;
    let mut child = Process::new(&exe)
        .arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|err| {
            io::Error::new(err.kind(), format!("cannot start {}: {err}", exe.display()))
        })?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match forward(&Request::Status, endpoints) {
            Ok(_) => return Ok(()),
            Err(ForwardError::NotRunning) => {}
            Err(err) => return Err(io::Error::other(err)),
        }
        // A daemon that lost the start race exits 0; keep waiting for the winner.
        if let Some(status) = child.try_wait()?
            && !status.success()
        {
            return Err(io::Error::other(format!(
                "the daemon exited during startup ({status}); run `quick-translate --daemon` to see why"
            )));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the daemon did not start within 3 s",
            ));
        }
        thread::sleep(POLL);
    }
}

/// Sends `Quit` and waits until the daemon is gone. Returns whether one was running.
fn stop_daemon(endpoints: &Endpoints) -> Result<bool, ForwardError> {
    match forward(&Request::Quit, endpoints) {
        Ok(_) => {}
        Err(ForwardError::NotRunning) => return Ok(false),
        Err(err) => return Err(err),
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        match forward(&Request::Status, endpoints) {
            Err(ForwardError::NotRunning) => return Ok(true),
            Ok(_) | Err(_) => thread::sleep(POLL),
        }
    }
    Err(ForwardError::Io(io::Error::new(
        io::ErrorKind::TimedOut,
        "the daemon did not exit within 3 s",
    )))
}

pub fn quit() -> ExitCode {
    match stop_daemon(&Endpoints::from_env()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("quick-translate: the daemon is not running");
            ExitCode::SUCCESS
        }
        Err(err) => fail(err),
    }
}

pub fn status() -> ExitCode {
    let mut out = String::new();
    let code = match forward(&Request::Status, &Endpoints::from_env()) {
        Ok(Reply::Status(status)) => {
            let popup = if status.popup_visible {
                "visible"
            } else {
                "hidden"
            };
            out.push_str(&format!(
                "daemon: running (pid {}, version {})\npopup: {popup}\ncapabilities:\n",
                status.pid, status.version
            ));
            push_indented(&mut out, &status.capabilities);
            ExitCode::SUCCESS
        }
        Ok(Reply::Done) => return fail("unexpected reply to Status"),
        Err(ForwardError::NotRunning) => {
            out.push_str("daemon: not running\n");
            match probe_capabilities() {
                Some(report) => {
                    out.push_str("capabilities (probed now):\n");
                    push_indented(&mut out, &report);
                }
                None => out.push_str("capabilities: unknown (start the daemon to probe them)\n"),
            }
            ExitCode::SUCCESS
        }
        Err(err) => return fail(err),
    };
    // A closed pipe (`--status | head -1`) is not an error worth reporting.
    let _ = io::stdout().lock().write_all(out.as_bytes());
    code
}

fn push_indented(out: &mut String, text: &str) {
    for line in text.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
}

/// One Wayland registry roundtrip and the portal version, like the daemon's startup probe.
/// Bounded to [`PROBE_TIMEOUT`] so an unresponsive compositor or bus cannot hang `--status`.
fn probe_capabilities() -> Option<String> {
    if std::env::var_os("QT_HEADLESS").is_some_and(|v| v == "1") {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("qt-status-probe".into())
        .spawn(move || {
            let Ok(mut caps) = quick_translate_platform::wayland::probe() else {
                let _ = tx.send(None);
                return;
            };
            caps.compositor = quick_translate_core::Compositor::detect();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(runtime) = runtime {
                caps.portal_global_shortcuts = runtime.block_on(async {
                    let connection = zbus::Connection::session().await.ok()?;
                    portal::detect_version(&connection).await
                });
            }
            let _ = tx.send(Some(caps.to_string()));
        })
        .ok()?;
    rx.recv_timeout(PROBE_TIMEOUT).ok().flatten()
}

/// `--remove-integration`: stop the daemon, then delete the D9 files. Config and secrets stay.
pub fn remove_integration() -> ExitCode {
    if let Err(err) = stop_daemon(&Endpoints::from_env()) {
        return fail(err);
    }
    let removed =
        integration::IntegrationPaths::from_env().and_then(|paths| integration::remove(&paths));
    match removed {
        Ok(changes) => {
            for path in &changes.removed {
                println!("removed {}", path.display());
            }
            if changes.removed.is_empty() {
                println!("no desktop integration was installed");
            }
            ExitCode::SUCCESS
        }
        Err(err) => fail(format!("cannot remove desktop integration: {err}")),
    }
}
