#![allow(clippy::unwrap_used)]

//! The example base URLs in docs/backends.md must pass transport validation (task 7.6).

use quick_translate_backends::{EndpointError, check_endpoint};

const DOC: &str = include_str!("../../../docs/backends.md");

/// Backticked `http(s)://` URLs on lines matching `pred`.
fn urls(pred: impl Fn(&str) -> bool) -> Vec<&'static str> {
    DOC.lines()
        .filter(|l| pred(l))
        .flat_map(|l| l.split('`').skip(1).step_by(2))
        .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
        .collect()
}

#[test]
fn example_base_urls_pass_without_confirmation() {
    let examples = urls(|l| l.starts_with('|'));
    for expected in [
        "https://api.openai.com/v1",
        "https://openrouter.ai/api/v1",
        "http://localhost:11434/v1",
    ] {
        assert!(
            examples.contains(&expected),
            "{expected} missing from the examples table"
        );
    }
    for url in &examples {
        assert!(check_endpoint(url, false).is_ok(), "{url} must be accepted");
    }
}

#[test]
fn documented_lan_example_needs_confirmation() {
    let lan = urls(|l| !l.starts_with('|') && l.contains("LAN"));
    assert!(!lan.is_empty());
    for url in lan {
        assert_eq!(
            check_endpoint(url, false),
            Err(EndpointError::NeedsConfirmation),
            "{url}"
        );
        assert!(check_endpoint(url, true).is_ok());
    }
}
