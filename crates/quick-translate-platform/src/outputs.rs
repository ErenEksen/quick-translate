//! Output enumeration on a dedicated Wayland connection.
//!
//! The layer shell creates one overlay per output and targets each by its `wl_output` name
//! (version 4). The toolkit does not report outputs, so a thread keeps the list current; it
//! blocks on the socket and only wakes when the compositor sends output events.

use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

use crate::wayland::{ProbeError, WaylandDisplay};

/// `wl_output` version that carries the `name` event.
const NAME_VERSION: u32 = 4;

/// One output as advertised by the compositor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputInfo {
    /// Registry name of the global.
    pub global: u32,
    /// Connector name such as `DP-1`, when the compositor sent it.
    pub name: Option<String>,
}

/// Keeps the output list of the compositor current.
#[derive(Debug, Clone)]
pub struct OutputTracker {
    shared: Arc<Mutex<Vec<OutputInfo>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum OutputError {
    #[error(transparent)]
    Connect(#[from] ProbeError),
    #[error("output enumeration failed: {0}")]
    Wayland(String),
    #[error("cannot start the output thread: {0}")]
    Thread(#[source] std::io::Error),
}

impl OutputTracker {
    /// Starts tracking on the display named by the environment.
    pub fn spawn() -> Result<Self, OutputError> {
        Self::spawn_on(&WaylandDisplay::from_env()?)
    }

    /// Starts tracking on `display`. Returns once the initial outputs and their names are known.
    pub fn spawn_on(display: &WaylandDisplay) -> Result<Self, OutputError> {
        let conn = display.connect()?;
        let (globals, mut queue) =
            registry_queue_init::<State>(&conn).map_err(|e| OutputError::Wayland(e.to_string()))?;
        let qh = queue.handle();
        let shared = Arc::new(Mutex::new(Vec::new()));
        let mut state = State {
            outputs: Vec::new(),
            shared: shared.clone(),
        };
        let advertised: Vec<(u32, u32)> = globals.contents().with_list(|list| {
            list.iter()
                .filter(|g| g.interface == WlOutput::interface().name)
                .map(|g| (g.name, g.version))
                .collect()
        });
        for (name, version) in advertised {
            state.bind(globals.registry(), name, version, &qh);
        }
        queue
            .roundtrip(&mut state)
            .map_err(|e| OutputError::Wayland(e.to_string()))?;
        state.publish();
        thread::Builder::new()
            .name("qt-outputs".into())
            .spawn(move || {
                let _conn = conn;
                while queue.blocking_dispatch(&mut state).is_ok() {}
                tracing::debug!("output thread stopped: Wayland connection closed");
            })
            .map_err(OutputError::Thread)?;
        Ok(Self { shared })
    }

    /// A tracker with a fixed list, for tests.
    pub fn fixed(outputs: Vec<OutputInfo>) -> Self {
        Self {
            shared: Arc::new(Mutex::new(outputs)),
        }
    }

    /// The current outputs, in the order the compositor advertised them.
    pub fn outputs(&self) -> Vec<OutputInfo> {
        lock(&self.shared).clone()
    }

    /// Names of the current outputs, or `None` when any output has no name (then the
    /// compositor has to choose the output).
    pub fn names(&self) -> Option<Vec<String>> {
        let outputs = self.outputs();
        if outputs.is_empty() {
            return None;
        }
        outputs.into_iter().map(|o| o.name).collect()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Tracked {
    global: u32,
    output: WlOutput,
    name: Option<String>,
}

struct State {
    outputs: Vec<Tracked>,
    shared: Arc<Mutex<Vec<OutputInfo>>>,
}

impl State {
    fn bind(&mut self, registry: &WlRegistry, global: u32, version: u32, qh: &QueueHandle<Self>) {
        let output = registry.bind::<WlOutput, _, _>(global, version.min(NAME_VERSION), qh, global);
        self.outputs.push(Tracked {
            global,
            output,
            name: None,
        });
    }

    fn publish(&self) {
        let list = self
            .outputs
            .iter()
            .map(|t| OutputInfo {
                global: t.global,
                name: t.name.clone(),
            })
            .collect();
        tracing::debug!(outputs = ?list, "outputs");
        *lock(&self.shared) = list;
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == WlOutput::interface().name => {
                state.bind(registry, name, version, qh);
            }
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(index) = state.outputs.iter().position(|t| t.global == name) {
                    let removed = state.outputs.remove(index);
                    if removed.output.version() >= 3 {
                        removed.output.release();
                    }
                    state.publish();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_output::Event::Name { name } => {
                if let Some(tracked) = state.outputs.iter_mut().find(|t| t.global == *global) {
                    tracked.name = Some(name);
                }
            }
            wl_output::Event::Done => state.publish(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_require_every_output_named() {
        let named = OutputTracker::fixed(vec![
            OutputInfo {
                global: 3,
                name: Some("DP-1".into()),
            },
            OutputInfo {
                global: 9,
                name: Some("HDMI-A-1".into()),
            },
        ]);
        assert_eq!(
            named.names(),
            Some(vec!["DP-1".to_owned(), "HDMI-A-1".to_owned()])
        );
        let partial = OutputTracker::fixed(vec![OutputInfo {
            global: 3,
            name: None,
        }]);
        assert_eq!(partial.names(), None);
        assert_eq!(OutputTracker::fixed(Vec::new()).names(), None);
    }
}
