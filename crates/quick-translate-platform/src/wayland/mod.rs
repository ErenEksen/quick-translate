//! Wayland registry probe and data-control thread (design D2, D5).
//!
//! [`probe`] runs one registry roundtrip on a dedicated connection and binds nothing. The
//! daemon fills [`Capabilities::compositor`] (via `Compositor::detect()`) and
//! [`Capabilities::portal_global_shortcuts`] itself; the probe leaves them at their defaults.

mod data_control;

use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use quick_translate_core::Capabilities;
use quick_translate_core::capabilities::DataControl;
use wayland_client::globals::{GlobalError, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};

pub use data_control::{
    BoundGlobal, DataControlHandle, MAX_READ_BYTES, OfferInfo, ReadError, Selection,
    SelectionSnapshot, SpawnError,
};

/// Interfaces that inject input or remote-control the session. They are never bound (spec
/// selection-capture, "No input injection or selection modification").
pub const FORBIDDEN_INTERFACES: &[&str] = &[
    "zwp_virtual_keyboard_manager_v1",
    "zwlr_virtual_pointer_manager_v1",
    "zwp_input_method_manager_v2",
    "org_kde_kwin_fake_input",
    "ext_input_capture_v1",
];

/// The Wayland socket the daemon talks to, resolved from `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR`.
///
/// Connections are always opened by path so that every subsystem gets its own connection;
/// `WAYLAND_SOCKET` is deliberately ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaylandDisplay {
    name: String,
    path: PathBuf,
}

impl WaylandDisplay {
    pub fn from_env() -> Result<Self, ProbeError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn from_lookup(env: impl Fn(&str) -> Option<String>) -> Result<Self, ProbeError> {
        let name = env("WAYLAND_DISPLAY")
            .filter(|n| !n.is_empty())
            .ok_or(ProbeError::NoDisplay)?;
        if Path::new(&name).is_absolute() {
            let path = PathBuf::from(&name);
            return Ok(Self { name, path });
        }
        let runtime_dir = env("XDG_RUNTIME_DIR")
            .filter(|d| !d.is_empty())
            .ok_or_else(|| ProbeError::NoRuntimeDir {
                display: name.clone(),
            })?;
        Ok(Self::new(runtime_dir, &name))
    }

    pub fn new(runtime_dir: impl Into<PathBuf>, name: &str) -> Self {
        Self {
            name: name.to_owned(),
            path: runtime_dir.into().join(name),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Open a new connection to this display.
    pub fn connect(&self) -> Result<Connection, ProbeError> {
        let connect_err = |source| ProbeError::Connect {
            socket: self.path.clone(),
            source,
        };
        let stream = UnixStream::connect(&self.path).map_err(connect_err)?;
        Connection::from_socket(stream).map_err(|e| connect_err(io::Error::other(e)))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("WAYLAND_DISPLAY is not set; quick-translate requires a Wayland session")]
    NoDisplay,
    #[error(
        "XDG_RUNTIME_DIR is not set, so the Wayland socket {display:?} cannot be located; \
         quick-translate requires a Wayland session"
    )]
    NoRuntimeDir { display: String },
    #[error("cannot connect to the Wayland socket {}: {source}", socket.display())]
    Connect {
        socket: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Wayland registry roundtrip on {} failed: {source}", socket.display())]
    Registry {
        socket: PathBuf,
        #[source]
        source: GlobalError,
    },
}

impl ProbeError {
    /// The socket path that was tried, if a connection was attempted.
    pub fn socket(&self) -> Option<&Path> {
        match self {
            Self::Connect { socket, .. } | Self::Registry { socket, .. } => Some(socket),
            Self::NoDisplay | Self::NoRuntimeDir { .. } => None,
        }
    }
}

/// Probe the compositor named by the environment.
pub fn probe() -> Result<Capabilities, ProbeError> {
    probe_display(&WaylandDisplay::from_env()?)
}

/// Probe a specific display with one registry roundtrip on a dedicated connection.
pub fn probe_display(display: &WaylandDisplay) -> Result<Capabilities, ProbeError> {
    let conn = display.connect()?;
    let (globals, _queue) =
        registry_queue_init::<ProbeState>(&conn).map_err(|source| ProbeError::Registry {
            socket: display.path.clone(),
            source,
        })?;
    let caps = globals.contents().with_list(|list| {
        capabilities_from_globals(list.iter().map(|g| (g.interface.as_str(), g.version)))
    });
    let socket = display.path.display();
    tracing::debug!(%socket, ?caps, "wayland probe");
    Ok(caps)
}

/// Build capabilities from advertised `(interface, version)` pairs, keeping the highest version.
pub fn capabilities_from_globals<'a>(
    globals: impl IntoIterator<Item = (&'a str, u32)>,
) -> Capabilities {
    let mut caps = Capabilities::default();
    for (interface, version) in globals {
        let slot = match interface {
            "zwlr_layer_shell_v1" => &mut caps.layer_shell,
            "ext_data_control_manager_v1" => &mut caps.ext_data_control,
            "zwlr_data_control_manager_v1" => &mut caps.wlr_data_control,
            "xdg_activation_v1" => &mut caps.xdg_activation,
            "wp_fractional_scale_manager_v1" => &mut caps.fractional_scale,
            "wp_viewporter" => &mut caps.viewporter,
            _ => continue,
        };
        *slot = Some(slot.map_or(version, |v| v.max(version)));
    }
    caps
}

/// The data-control protocol to use for background reads: ext-data-control-v1, else
/// wlr-data-control v2 or later (v1 has no PRIMARY).
pub fn usable_data_control(caps: &Capabilities) -> Option<DataControl> {
    if let Some(v) = caps.ext_data_control {
        return Some(DataControl::Ext(v));
    }
    caps.wlr_data_control
        .filter(|&v| v >= 2)
        .map(DataControl::Wlr)
}

struct ProbeState;

impl Dispatch<WlRegistry, GlobalListContents> for ProbeState {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn display_from_env() {
        let d = WaylandDisplay::from_lookup(env(&[
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]))
        .unwrap();
        assert_eq!(d.name(), "wayland-1");
        assert_eq!(d.path(), Path::new("/run/user/1000/wayland-1"));

        let abs = WaylandDisplay::from_lookup(env(&[("WAYLAND_DISPLAY", "/tmp/sock")])).unwrap();
        assert_eq!(abs.path(), Path::new("/tmp/sock"));
    }

    #[test]
    fn display_missing_env() {
        assert!(matches!(
            WaylandDisplay::from_lookup(env(&[])),
            Err(ProbeError::NoDisplay)
        ));
        assert!(matches!(
            WaylandDisplay::from_lookup(env(&[("WAYLAND_DISPLAY", "")])),
            Err(ProbeError::NoDisplay)
        ));
        assert!(matches!(
            WaylandDisplay::from_lookup(env(&[("WAYLAND_DISPLAY", "wayland-0")])),
            Err(ProbeError::NoRuntimeDir { .. })
        ));
    }

    #[test]
    fn connect_error_names_socket() {
        let dir = tempfile::tempdir().unwrap();
        let display = WaylandDisplay::new(dir.path(), "wayland-404");
        let err = display.connect().unwrap_err();
        assert_eq!(err.socket(), Some(display.path()));
        assert!(err.to_string().contains("wayland-404"), "{err}");
    }

    #[test]
    fn caps_from_globals() {
        let caps = capabilities_from_globals([
            ("wl_compositor", 6),
            ("zwlr_layer_shell_v1", 4),
            ("ext_data_control_manager_v1", 1),
            ("zwlr_data_control_manager_v1", 2),
            ("xdg_activation_v1", 1),
            ("wp_fractional_scale_manager_v1", 1),
            ("wp_viewporter", 1),
            ("wp_viewporter", 2),
        ]);
        assert_eq!(caps.layer_shell, Some(4));
        assert_eq!(caps.ext_data_control, Some(1));
        assert_eq!(caps.wlr_data_control, Some(2));
        assert_eq!(caps.xdg_activation, Some(1));
        assert_eq!(caps.fractional_scale, Some(1));
        assert_eq!(caps.viewporter, Some(2));
        assert_eq!(caps.portal_global_shortcuts, None);
        assert_eq!(capabilities_from_globals([]), Capabilities::default());
    }

    #[test]
    fn data_control_preference() {
        let mut caps = Capabilities {
            ext_data_control: Some(1),
            wlr_data_control: Some(2),
            ..Default::default()
        };
        assert_eq!(usable_data_control(&caps), Some(DataControl::Ext(1)));
        caps.ext_data_control = None;
        assert_eq!(usable_data_control(&caps), Some(DataControl::Wlr(2)));
        caps.wlr_data_control = Some(1);
        assert_eq!(usable_data_control(&caps), None);
        caps.wlr_data_control = None;
        assert_eq!(usable_data_control(&caps), None);
    }
}
