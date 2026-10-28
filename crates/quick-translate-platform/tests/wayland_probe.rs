//! Tasks 5.1 and 5.4: registry probe and the set of bound globals, against headless sway.
//!
//! ```sh
//! eval "$(scripts/headless-sway.sh start)"
//! cargo test -p quick-translate-platform --test wayland_probe
//! ```

mod wayland_support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use quick_translate_core::capabilities::DataControl;
use quick_translate_platform::wayland::{
    self, DataControlHandle, FORBIDDEN_INTERFACES, WaylandDisplay,
};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_support::headless;

/// Globals the data-control thread may bind.
const ALLOWED_BINDS: &[&str] = &[
    "wl_seat",
    "ext_data_control_manager_v1",
    "zwlr_data_control_manager_v1",
];

struct Registry;

impl Dispatch<WlRegistry, GlobalListContents> for Registry {
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

fn advertised(display: &WaylandDisplay) -> BTreeMap<String, u32> {
    let conn = display.connect().expect("connect");
    let (globals, _queue) = registry_queue_init::<Registry>(&conn).expect("registry");
    globals
        .contents()
        .clone_list()
        .into_iter()
        .map(|g| (g.interface, g.version))
        .collect()
}

fn protocols(display: &WaylandDisplay) -> Vec<DataControl> {
    let caps = wayland::probe_display(display).expect("probe");
    let mut out = Vec::new();
    if let Some(v) = caps.ext_data_control {
        out.push(DataControl::Ext(v));
    }
    if let Some(v) = caps.wlr_data_control.filter(|&v| v >= 2) {
        out.push(DataControl::Wlr(v));
    }
    out
}

#[test]
fn probe_detects_layer_shell_and_data_control() {
    let Some(display) = headless("probe_detects_layer_shell_and_data_control") else {
        return;
    };
    let caps = wayland::probe_display(&display).expect("probe");
    eprintln!("headless sway capabilities: {caps:?}");
    assert!(caps.layer_shell.is_some(), "layer-shell not detected");
    assert!(
        wayland::usable_data_control(&caps).is_some(),
        "no usable data-control detected: {caps:?}"
    );
    assert!(caps.xdg_activation.is_some());
    assert!(caps.viewporter.is_some());
    assert_eq!(caps.portal_global_shortcuts, None);

    let globals = advertised(&display);
    assert_eq!(
        caps.layer_shell,
        globals.get("zwlr_layer_shell_v1").copied()
    );
    assert_eq!(
        caps.ext_data_control,
        globals.get("ext_data_control_manager_v1").copied()
    );
    assert_eq!(
        caps.wlr_data_control,
        globals.get("zwlr_data_control_manager_v1").copied()
    );
}

#[test]
fn data_control_binds_no_input_injection_protocol() {
    let Some(display) = headless("data_control_binds_no_input_injection_protocol") else {
        return;
    };
    let globals = advertised(&display);
    let tempting: Vec<_> = FORBIDDEN_INTERFACES
        .iter()
        .filter(|i| globals.contains_key(**i))
        .collect();
    eprintln!("input-injection globals advertised by the compositor: {tempting:?}");

    let protocols = protocols(&display);
    assert!(!protocols.is_empty());
    for protocol in protocols {
        let handle = DataControlHandle::spawn_on(&display, protocol).expect("spawn");
        let bound = handle.bound_globals();
        eprintln!("{protocol:?} bound {bound:?}");
        assert!(bound.iter().any(|b| b.interface == "wl_seat"));
        for b in &bound {
            assert!(
                ALLOWED_BINDS.contains(&b.interface),
                "unexpected global bound: {b:?}"
            );
            assert!(!FORBIDDEN_INTERFACES.contains(&b.interface));
        }
    }
}

/// `--ignored`: read-only probe of the session in `WAYLAND_DISPLAY` (e.g. the developer's
/// desktop). Binds nothing.
#[test]
#[ignore = "probes the current session; run manually"]
fn probe_current_session() {
    let caps = wayland::probe().expect("probe");
    eprintln!(
        "{caps:?}\nusable data-control: {:?}",
        wayland::usable_data_control(&caps)
    );
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs" || e == "toml") {
            out.push(path);
        }
    }
}

/// No source file or manifest in the workspace mentions an input-injection API. The single
/// allowed mention is the deny-list itself.
#[test]
fn no_input_injection_api_in_sources() {
    let root = workspace_root();
    let mut files = vec![root.join("Cargo.toml")];
    for krate in std::fs::read_dir(root.join("crates"))
        .expect("crates dir")
        .flatten()
    {
        files.push(krate.path().join("Cargo.toml"));
        collect(&krate.path().join("src"), &mut files);
    }
    let deny_list = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/wayland/mod.rs");
    // Built at runtime so this file does not match itself.
    let needles: Vec<String> = [
        ["virtual", "_keyboard"],
        ["virtual", "_pointer"],
        ["remote", "_desktop"],
        ["Remote", "Desktop"],
        ["fake", "_input"],
        ["uin", "put"],
        ["Virtual", "Device"],
        ["xdo", "tool"],
        ["ydo", "tool"],
    ]
    .iter()
    .map(|p| p.concat())
    .collect();
    let mut hits = Vec::new();
    for file in files.iter().filter(|f| **f != deny_list) {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for needle in &needles {
            if text.contains(needle.as_str()) {
                hits.push(format!("{}: {needle}", file.display()));
            }
        }
    }
    assert!(
        files.len() > 5,
        "source scan found too few files: {files:?}"
    );
    assert!(
        hits.is_empty(),
        "input-injection APIs referenced: {hits:#?}"
    );
}
