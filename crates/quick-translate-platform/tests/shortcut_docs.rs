//! Every generated snippet must appear verbatim in docs/shortcuts.md.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use quick_translate_platform::shortcut::snippet::{SnippetFormat, ToggleCommand, generate};

const DOC_EXE: &str = "/home/you/Applications/QuickTranslate-x86_64.AppImage";

#[test]
fn docs_contain_every_snippet() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/shortcuts.md");
    let docs = std::fs::read_to_string(path).expect("docs/shortcuts.md exists");
    let cmd = ToggleCommand::new(DOC_EXE);
    for format in SnippetFormat::ALL {
        let snippet = generate(format, &cmd);
        assert!(
            docs.contains(&format!("```\n{}\n```", snippet.text))
                || docs.contains(&format!("\n{}\n```", snippet.text)),
            "docs/shortcuts.md is missing the {} snippet:\n{}",
            format.name(),
            snippet.text
        );
        assert!(
            docs.contains(&format!("## {}", format.name())),
            "no section for {}",
            format.name()
        );
    }
}
