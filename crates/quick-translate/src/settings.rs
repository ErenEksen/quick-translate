//! The settings window: a separate process the daemon spawns on `Settings()` (design D4), and
//! the daemon-side helpers its D-Bus calls need.

use std::io;
use std::process::{ExitCode, Stdio};

use quick_translate_ipc::{ShortcutInfo, ShortcutMechanism, SnippetInfo};
use quick_translate_platform::shortcut::{Mechanism, evdev};

use crate::cli::SETTINGS_WINDOW_FLAG;

/// `--settings-window`: runs the settings UI until its window closes.
pub fn run_window() -> ExitCode {
    crate::logging::init();
    match quick_translate_ui::settings::run(quick_translate_ui::settings::RunOptions::default()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("quick-translate: cannot open the settings window: {err}");
            ExitCode::FAILURE
        }
    }
}

/// The settings process owned by the daemon. At most one runs; it is killed when this value is
/// dropped (daemon exit).
#[derive(Debug, Default)]
pub struct SettingsWindow {
    child: Option<tokio::process::Child>,
}

/// Outcome of [`SettingsWindow::open`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    Spawned,
    /// A window is already open; the daemon emits `SettingsRequested` so it raises itself.
    AlreadyOpen,
}

impl SettingsWindow {
    /// Spawns `<exe> --settings-window` unless one is still running. Must run inside Tokio.
    pub fn open(&mut self) -> io::Result<Opened> {
        if let Some(child) = &mut self.child {
            if let Ok(None) = child.try_wait() {
                return Ok(Opened::AlreadyOpen);
            }
            self.child = None;
        }
        // The daemon's own binary: inside an AppImage its mount lives as long as the daemon,
        // which also outlives (and kills) this child.
        let exe = std::env::current_exe()?;
        let child = tokio::process::Command::new(&exe)
            .arg(SETTINGS_WINDOW_FLAG)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| {
                io::Error::new(err.kind(), format!("cannot start {}: {err}", exe.display()))
            })?;
        self.child = Some(child);
        Ok(Opened::Spawned)
    }
}

/// The `ShortcutInfo()` reply for the active mechanism and evdev listener state.
pub fn shortcut_info(
    mechanism: &Mechanism,
    evdev_active: bool,
    evdev_error: Option<String>,
) -> ShortcutInfo {
    let mut info = ShortcutInfo {
        label: mechanism.label().to_owned(),
        evdev_active,
        evdev_error,
        evdev_warning: evdev::WARNING.to_owned(),
        ..ShortcutInfo::default()
    };
    match mechanism {
        Mechanism::Portal(portal) => {
            info.mechanism = ShortcutMechanism::Portal;
            info.portal_version = portal.version();
            info.trigger = portal.current_trigger();
            info.can_configure = portal.can_configure();
        }
        Mechanism::Compositor(snippet) => {
            info.mechanism = ShortcutMechanism::Compositor;
            info.snippet = Some(SnippetInfo {
                name: snippet.format.name().to_owned(),
                instructions: snippet.instructions.clone(),
                text: snippet.text.clone(),
            });
        }
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use quick_translate_core::Compositor;
    use quick_translate_platform::shortcut::{ToggleCommand, snippet};

    #[test]
    fn compositor_info_carries_the_snippet() {
        let snippet =
            snippet::snippet_for(&Compositor::Niri, &ToggleCommand::new("/opt/qt.AppImage"));
        let info = shortcut_info(
            &Mechanism::Compositor(snippet.clone()),
            false,
            Some("no permission".into()),
        );
        assert_eq!(info.mechanism, ShortcutMechanism::Compositor);
        assert!(
            info.label.starts_with("Compositor binding"),
            "{}",
            info.label
        );
        let carried = info.snippet.expect("snippet");
        assert_eq!(carried.name, "niri");
        assert_eq!(carried.text, snippet.text);
        assert!(carried.text.contains("/opt/qt.AppImage"));
        assert_eq!(info.evdev_error.as_deref(), Some("no permission"));
        assert_eq!(info.evdev_warning, evdev::WARNING);
        assert!(!info.can_configure);
    }
}
