//! Output tracking for the layer shell, against headless sway.
//!
//! ```sh
//! eval "$(scripts/headless-sway.sh start)"
//! cargo test -p quick-translate-platform --test wayland_outputs
//! ```

mod wayland_support;

use quick_translate_platform::outputs::OutputTracker;
use wayland_support::headless;

#[test]
fn headless_outputs_have_names() {
    let Some(display) = headless("headless_outputs_have_names") else {
        return;
    };
    let tracker = OutputTracker::spawn_on(&display).expect("output tracker");
    let names = tracker.names().expect("every output is named");
    assert!(!names.is_empty());
    assert!(
        names.iter().all(|n| n.starts_with("HEADLESS-")),
        "{names:?}"
    );
}
