//! Logging to stderr, filtered by `QT_LOG`.
//!
//! `QT_LOG` is an `EnvFilter`. A bare level such as `QT_LOG=debug` applies to the
//! quick-translate crates only and keeps every other crate at `warn`, so the hot-path timings
//! are not buried in toolkit and GPU driver output. Unset means `info` for our crates.

use tracing_subscriber::EnvFilter;

const CRATES: [&str; 7] = [
    "quick_translate",
    "quick_translate_core",
    "quick_translate_platform",
    "quick_translate_backends",
    "quick_translate_secrets",
    "quick_translate_ipc",
    "quick_translate_ui",
];

const LEVELS: [&str; 6] = ["off", "error", "warn", "info", "debug", "trace"];

pub fn init() {
    let value = std::env::var("QT_LOG").ok();
    let filter = EnvFilter::try_new(directives(value.as_deref()))
        .unwrap_or_else(|_| EnvFilter::new(directives(None)));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn directives(value: Option<&str>) -> String {
    let value = value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("info");
    let level = value.to_ascii_lowercase();
    if !LEVELS.contains(&level.as_str()) {
        return value.to_owned();
    }
    let ours: Vec<String> = CRATES.iter().map(|c| format!("{c}={level}")).collect();
    format!("warn,{}", ours.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_level_applies_to_our_crates_only() {
        let d = directives(Some("debug"));
        assert!(d.starts_with("warn,"), "{d}");
        assert!(d.contains("quick_translate_ui=debug"), "{d}");
        assert!(d.contains("quick_translate=debug"), "{d}");
        assert_eq!(directives(Some("DEBUG")), d);
    }

    #[test]
    fn unset_is_info_and_full_filters_pass_through() {
        assert!(directives(None).contains("quick_translate=info"));
        assert!(directives(Some(" ")).contains("quick_translate=info"));
        assert_eq!(directives(Some("wgpu=debug,info")), "wgpu=debug,info");
        assert!(EnvFilter::try_new(directives(Some("trace"))).is_ok());
    }
}
