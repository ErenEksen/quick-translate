//! Opt-in input-device fallback (design D6).
//!
//! Reads `/dev/input/event*` keyboards read-only and without grabbing, so other clients
//! keep receiving every key. It can see all keyboard input, which is why it stays off until
//! the user confirmed [`WARNING`], represented by a [`Confirmed`] value.

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
};

use evdev::{Device, EventSummary, KeyCode};
use futures::Stream;
use tokio::{sync::mpsc, task::JoinHandle};

use super::Activation;

/// Shown before the user turns the fallback on.
pub const WARNING: &str = "The input-device fallback reads your keyboard devices directly. \
While it is on, Quick Translate can see everything you type, in every application. \
Only turn it on if your desktop has no shortcut portal and you cannot add a compositor binding.";

pub const DEFAULT_INPUT_DIR: &str = "/dev/input";

/// Proof that the user accepted [`WARNING`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Confirmed(());

impl Confirmed {
    /// Create only after the user explicitly confirmed [`WARNING`], or from a persisted
    /// opt-in that was itself saved after that confirmation.
    pub fn user_accepted_warning() -> Self {
        Self(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EvdevMode {
    #[default]
    Disabled,
    Enabled(Confirmed),
}

impl EvdevMode {
    /// Mode from the persisted `evdev_enabled` flag, which settings only sets after the
    /// confirmation.
    pub fn from_saved_opt_in(enabled: bool) -> Self {
        if enabled {
            Self::Enabled(Confirmed::user_accepted_warning())
        } else {
            Self::Disabled
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EvdevError {
    #[error("the input-device fallback is disabled")]
    Disabled,
    #[error(
        "no permission to read keyboard input devices ({}); add your user to the `input` \
         group and log in again",
        path.display()
    )]
    PermissionDenied { path: PathBuf },
    #[error("no keyboard input devices found in {}", dir.display())]
    NoKeyboards { dir: PathBuf },
    #[error("cannot read input devices in {}: {source}", dir.display())]
    Io {
        dir: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// A key combination: exact modifier set plus one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Combo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    pub key: KeyCode,
}

impl Combo {
    pub const CTRL_ALT_C: Self = Self {
        ctrl: true,
        alt: true,
        shift: false,
        meta: false,
        key: KeyCode::KEY_C,
    };
}

impl Default for Combo {
    fn default() -> Self {
        Self::CTRL_ALT_C
    }
}

const RELEASE: i32 = 0;
const PRESS: i32 = 1;
const REPEAT: i32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Meta,
}

fn modifier(key: KeyCode) -> Option<(Modifier, usize)> {
    Some(match key {
        KeyCode::KEY_LEFTCTRL => (Modifier::Ctrl, 0),
        KeyCode::KEY_RIGHTCTRL => (Modifier::Ctrl, 1),
        KeyCode::KEY_LEFTALT => (Modifier::Alt, 0),
        KeyCode::KEY_RIGHTALT => (Modifier::Alt, 1),
        KeyCode::KEY_LEFTSHIFT => (Modifier::Shift, 0),
        KeyCode::KEY_RIGHTSHIFT => (Modifier::Shift, 1),
        KeyCode::KEY_LEFTMETA => (Modifier::Meta, 0),
        KeyCode::KEY_RIGHTMETA => (Modifier::Meta, 1),
        _ => return None,
    })
}

/// Pure key-combo state machine fed with `EV_KEY` events (value 0 release, 1 press,
/// 2 autorepeat). Fires once per physical press of the combo key while exactly the
/// combo's modifiers are held; autorepeat never fires.
#[derive(Debug, Clone)]
pub struct ComboDetector {
    combo: Combo,
    /// `[left, right]` held state for ctrl, alt, shift, meta.
    held: [[bool; 2]; 4],
    key_down: bool,
}

impl ComboDetector {
    pub fn new(combo: Combo) -> Self {
        Self {
            combo,
            held: [[false; 2]; 4],
            key_down: false,
        }
    }

    /// Forget all held keys, e.g. after the device dropped events.
    pub fn reset(&mut self) {
        self.held = [[false; 2]; 4];
        self.key_down = false;
    }

    fn is_held(&self, m: Modifier) -> bool {
        self.held[m as usize].iter().any(|&h| h)
    }

    fn modifiers_match(&self) -> bool {
        self.is_held(Modifier::Ctrl) == self.combo.ctrl
            && self.is_held(Modifier::Alt) == self.combo.alt
            && self.is_held(Modifier::Shift) == self.combo.shift
            && self.is_held(Modifier::Meta) == self.combo.meta
    }

    /// Feed one key event. Returns `true` when the combo fires.
    pub fn handle(&mut self, key: KeyCode, value: i32) -> bool {
        if let Some((m, side)) = modifier(key) {
            match value {
                PRESS | REPEAT => self.held[m as usize][side] = true,
                RELEASE => self.held[m as usize][side] = false,
                _ => {}
            }
            return false;
        }
        if key != self.combo.key {
            return false;
        }
        match value {
            PRESS => {
                let first = !self.key_down;
                self.key_down = true;
                first && self.modifiers_match()
            }
            REPEAT => {
                self.key_down = true;
                false
            }
            RELEASE => {
                self.key_down = false;
                false
            }
            _ => false,
        }
    }
}

/// Running listener. Yields one [`Activation`] (without a token) per combo press.
/// Dropping it stops reading the devices.
pub struct EvdevListener {
    rx: mpsc::UnboundedReceiver<Activation>,
    tasks: Vec<JoinHandle<()>>,
    devices: Vec<PathBuf>,
}

impl EvdevListener {
    /// Start on `/dev/input` with Ctrl+Alt+C. Must run inside a Tokio runtime.
    pub fn start(mode: EvdevMode) -> Result<Self, EvdevError> {
        Self::start_in(mode, Path::new(DEFAULT_INPUT_DIR), Combo::default())
    }

    /// Start on the `event*` devices in `dir`. Returns [`EvdevError::Disabled`] without
    /// touching `dir` unless `mode` is enabled.
    pub fn start_in(mode: EvdevMode, dir: &Path, combo: Combo) -> Result<Self, EvdevError> {
        let EvdevMode::Enabled(_) = mode else {
            return Err(EvdevError::Disabled);
        };
        let io_err = |source: io::Error| match source.kind() {
            io::ErrorKind::PermissionDenied => EvdevError::PermissionDenied { path: dir.into() },
            _ => EvdevError::Io {
                dir: dir.into(),
                source,
            },
        };

        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(io_err)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("event"))
            })
            .collect();
        paths.sort();

        let mut denied = None;
        let mut keyboards = Vec::new();
        for path in paths {
            match open_read_only(&path) {
                Ok(device) if is_keyboard(&device, combo) => keyboards.push((path, device)),
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                    denied.get_or_insert(path);
                }
                Err(err) => tracing::debug!("skipping {}: {err}", path.display()),
            }
        }
        if keyboards.is_empty() {
            return Err(match denied {
                Some(path) => EvdevError::PermissionDenied { path },
                None => EvdevError::NoKeyboards { dir: dir.into() },
            });
        }

        let (tx, rx) = mpsc::unbounded_channel();
        let mut tasks = Vec::with_capacity(keyboards.len());
        let mut devices = Vec::with_capacity(keyboards.len());
        for (path, device) in keyboards {
            let mut stream = device.into_event_stream().map_err(io_err)?;
            let tx = tx.clone();
            let name = path.display().to_string();
            tasks.push(tokio::spawn(async move {
                let mut detector = ComboDetector::new(combo);
                loop {
                    match stream.next_event().await {
                        Ok(event) => {
                            if let EventSummary::Key(_, key, value) = event.destructure()
                                && detector.handle(key, value)
                                && tx.send(Activation::default()).is_err()
                            {
                                break;
                            }
                        }
                        Err(err) => {
                            tracing::warn!("stopped reading {name}: {err}");
                            break;
                        }
                    }
                }
            }));
            devices.push(path);
        }
        tracing::info!(
            "input-device fallback listening on {} keyboard(s)",
            devices.len()
        );
        Ok(Self { rx, tasks, devices })
    }

    /// Keyboard devices being read.
    pub fn devices(&self) -> &[PathBuf] {
        &self.devices
    }

    pub async fn next(&mut self) -> Option<Activation> {
        self.rx.recv().await
    }
}

impl Stream for EvdevListener {
    type Item = Activation;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Activation>> {
        self.rx.poll_recv(cx)
    }
}

impl Drop for EvdevListener {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn open_read_only(path: &Path) -> io::Result<Device> {
    Device::from_fd(File::open(path)?.into())
}

fn is_keyboard(device: &Device, combo: Combo) -> bool {
    device.supported_keys().is_some_and(|keys| {
        keys.contains(combo.key)
            && (keys.contains(KeyCode::KEY_LEFTCTRL) || keys.contains(KeyCode::KEY_RIGHTCTRL))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: KeyCode = KeyCode::KEY_C;
    const LCTRL: KeyCode = KeyCode::KEY_LEFTCTRL;
    const RCTRL: KeyCode = KeyCode::KEY_RIGHTCTRL;
    const LALT: KeyCode = KeyCode::KEY_LEFTALT;
    const RALT: KeyCode = KeyCode::KEY_RIGHTALT;

    fn feed(d: &mut ComboDetector, events: &[(KeyCode, i32)]) -> usize {
        events.iter().filter(|&&(k, v)| d.handle(k, v)).count()
    }

    #[test]
    fn fires_for_every_side_combination() {
        for ctrl in [LCTRL, RCTRL] {
            for alt in [LALT, RALT] {
                let mut d = ComboDetector::new(Combo::CTRL_ALT_C);
                let fired = feed(&mut d, &[(ctrl, PRESS), (alt, PRESS), (C, PRESS)]);
                assert_eq!(fired, 1, "{ctrl:?} {alt:?}");
            }
        }
    }

    #[test]
    fn repeat_is_suppressed_and_release_rearms() {
        let mut d = ComboDetector::new(Combo::CTRL_ALT_C);
        let events = [
            (LCTRL, PRESS),
            (LALT, PRESS),
            (C, PRESS),
            (C, REPEAT),
            (C, REPEAT),
            (C, RELEASE),
            (C, PRESS),
            (C, RELEASE),
        ];
        assert_eq!(feed(&mut d, &events), 2);
    }

    #[test]
    fn duplicate_press_without_release_fires_once() {
        let mut d = ComboDetector::new(Combo::CTRL_ALT_C);
        assert_eq!(
            feed(
                &mut d,
                &[(LCTRL, PRESS), (LALT, PRESS), (C, PRESS), (C, PRESS)]
            ),
            1
        );
    }

    #[test]
    fn requires_exact_modifiers() {
        let mut d = ComboDetector::new(Combo::CTRL_ALT_C);
        assert_eq!(feed(&mut d, &[(C, PRESS), (C, RELEASE)]), 0);
        assert_eq!(feed(&mut d, &[(LCTRL, PRESS), (C, PRESS), (C, RELEASE)]), 0);
        assert_eq!(
            feed(
                &mut d,
                &[
                    (LALT, PRESS),
                    (KeyCode::KEY_LEFTSHIFT, PRESS),
                    (C, PRESS),
                    (C, RELEASE)
                ]
            ),
            0
        );
        assert_eq!(
            feed(&mut d, &[(KeyCode::KEY_LEFTSHIFT, RELEASE), (C, PRESS)]),
            1
        );
    }

    #[test]
    fn modifier_release_on_one_side_keeps_other_side() {
        let mut d = ComboDetector::new(Combo::CTRL_ALT_C);
        let events = [
            (LCTRL, PRESS),
            (RCTRL, PRESS),
            (LALT, PRESS),
            (LCTRL, RELEASE),
            (C, PRESS),
        ];
        assert_eq!(feed(&mut d, &events), 1);
        assert_eq!(
            feed(&mut d, &[(C, RELEASE), (RCTRL, RELEASE), (C, PRESS)]),
            0
        );
    }

    #[test]
    fn key_held_before_modifiers_does_not_fire() {
        let mut d = ComboDetector::new(Combo::CTRL_ALT_C);
        assert_eq!(
            feed(
                &mut d,
                &[(C, PRESS), (LCTRL, PRESS), (LALT, PRESS), (C, REPEAT)]
            ),
            0
        );
        assert_eq!(feed(&mut d, &[(C, RELEASE), (C, PRESS)]), 1);
    }

    #[test]
    fn repeat_seen_first_counts_as_held() {
        let mut d = ComboDetector::new(Combo::CTRL_ALT_C);
        assert_eq!(
            feed(
                &mut d,
                &[(LCTRL, PRESS), (LALT, PRESS), (C, REPEAT), (C, PRESS)]
            ),
            0
        );
    }

    #[test]
    fn reset_clears_modifiers() {
        let mut d = ComboDetector::new(Combo::CTRL_ALT_C);
        feed(&mut d, &[(LCTRL, PRESS), (LALT, PRESS)]);
        d.reset();
        assert_eq!(feed(&mut d, &[(C, PRESS)]), 0);
    }

    #[test]
    fn default_mode_is_disabled() {
        assert_eq!(EvdevMode::default(), EvdevMode::Disabled);
        assert_eq!(EvdevMode::from_saved_opt_in(false), EvdevMode::Disabled);
    }
}
