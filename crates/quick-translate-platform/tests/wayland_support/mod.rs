//! Shared helpers for the headless-compositor integration tests.
//!
//! The tests connect only to the compositor named by `QT_TEST_XDG_RUNTIME_DIR` and
//! `QT_TEST_WAYLAND_DISPLAY` (see `scripts/headless-sway.sh`) and skip when those are unset.
#![allow(dead_code)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use quick_translate_platform::wayland::WaylandDisplay;

/// The headless test compositor, or `None` (with a skip notice) when it is not configured.
pub fn headless(test: &str) -> Option<WaylandDisplay> {
    let (Ok(dir), Ok(name)) = (
        std::env::var("QT_TEST_XDG_RUNTIME_DIR"),
        std::env::var("QT_TEST_WAYLAND_DISPLAY"),
    ) else {
        eprintln!(
            "skipping {test}: QT_TEST_XDG_RUNTIME_DIR/QT_TEST_WAYLAND_DISPLAY are unset \
             (run `eval \"$(scripts/headless-sway.sh start)\"` first)"
        );
        return None;
    };
    let display = WaylandDisplay::new(&dir, &name);
    if let Ok(live) = WaylandDisplay::from_env() {
        let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        };
        assert!(
            !same(live.path(), display.path()),
            "refusing to run {test} against the live session socket {}",
            live.path().display()
        );
    }
    Some(display)
}

/// Tests in one binary share the compositor's selections, so they run one at a time.
pub fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn command(display: &WaylandDisplay, program: &str) -> Command {
    let mut cmd = Command::new(program);
    let dir = display.path().parent().expect("socket path has a parent");
    cmd.env("XDG_RUNTIME_DIR", dir)
        .env("WAYLAND_DISPLAY", display.name())
        .env_remove("DISPLAY");
    cmd
}

/// Run `wl-copy` against the test compositor, feeding `stdin` if given.
pub fn wl_copy(display: &WaylandDisplay, args: &[&str], stdin: Option<&[u8]>) {
    let mut child = command(display, "wl-copy")
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        // wl-copy forks a server that outlives this call; it must not hold our pipes.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("wl-copy (wl-clipboard) must be installed to run the Wayland integration tests");
    if let Some(data) = stdin {
        let mut pipe = child.stdin.take().expect("piped stdin");
        pipe.write_all(data).expect("write to wl-copy");
    }
    let status = child.wait().expect("wait for wl-copy");
    assert!(status.success(), "wl-copy {args:?} failed: {status}");
}

/// Run `wl-paste` against the test compositor.
pub fn wl_paste(display: &WaylandDisplay, args: &[&str]) -> Output {
    command(display, "wl-paste")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("wl-paste (wl-clipboard) must be installed")
}

/// Poll `cond` until it holds or two seconds pass.
pub fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
