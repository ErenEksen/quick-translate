//! The input-device fallback never starts while disabled and reports missing permission.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{os::unix::fs::PermissionsExt, path::Path};

use quick_translate_platform::shortcut::evdev::{
    Combo, Confirmed, EvdevError, EvdevListener, EvdevMode,
};

fn enabled() -> EvdevMode {
    EvdevMode::Enabled(Confirmed::user_accepted_warning())
}

fn is_root() -> bool {
    rustix::process::geteuid().is_root()
}

#[test]
fn never_starts_while_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let device = dir.path().join("event0");
    std::fs::write(&device, "").unwrap();
    std::fs::set_permissions(&device, std::fs::Permissions::from_mode(0o000)).unwrap();

    for target in [
        dir.path(),
        Path::new("/dev/input"),
        Path::new("/nonexistent"),
    ] {
        let err = EvdevListener::start_in(EvdevMode::Disabled, target, Combo::default())
            .err()
            .expect("disabled listener must not start");
        assert!(matches!(err, EvdevError::Disabled), "{err}");
    }
    assert!(matches!(
        EvdevListener::start(EvdevMode::default()),
        Err(EvdevError::Disabled)
    ));
    assert!(matches!(
        EvdevListener::start(EvdevMode::from_saved_opt_in(false)),
        Err(EvdevError::Disabled)
    ));
}

#[test]
fn unreadable_device_reports_permission() {
    if is_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for name in ["event0", "event1"] {
        let device = dir.path().join(name);
        std::fs::write(&device, "").unwrap();
        std::fs::set_permissions(&device, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let err = EvdevListener::start_in(enabled(), dir.path(), Combo::default())
        .err()
        .unwrap();
    assert!(
        matches!(&err, EvdevError::PermissionDenied { path } if path.ends_with("event0")),
        "{err}"
    );
    assert!(err.to_string().contains("`input` group"), "{err}");
}

#[test]
fn unreadable_directory_reports_permission() {
    if is_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    std::fs::set_permissions(&input, std::fs::Permissions::from_mode(0o000)).unwrap();
    let err = EvdevListener::start_in(enabled(), &input, Combo::default())
        .err()
        .unwrap();
    std::fs::set_permissions(&input, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(err, EvdevError::PermissionDenied { .. }), "{err}");
}

#[test]
fn missing_or_empty_directory() {
    let err = EvdevListener::start_in(enabled(), Path::new("/nonexistent/input"), Combo::default())
        .err()
        .unwrap();
    assert!(matches!(err, EvdevError::Io { .. }), "{err}");

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("event0"), "not a device").unwrap();
    std::fs::write(dir.path().join("mouse0"), "").unwrap();
    let err = EvdevListener::start_in(enabled(), dir.path(), Combo::default())
        .err()
        .unwrap();
    assert!(matches!(err, EvdevError::NoKeyboards { .. }), "{err}");
}
