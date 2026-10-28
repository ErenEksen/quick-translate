//! Snapshot tests for the compositor-binding snippets.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use quick_translate_core::Compositor;
use quick_translate_platform::shortcut::snippet::{
    SnippetFormat, ToggleCommand, generate, snippet_for,
};

/// The path used in docs/shortcuts.md.
pub const DOC_EXE: &str = "/home/you/Applications/QuickTranslate-x86_64.AppImage";
const SPACED_EXE: &str = "/home/you/My Apps/Quick Translate's.AppImage";

fn snapshot_name(format: SnippetFormat) -> &'static str {
    match format {
        SnippetFormat::Sway => "sway",
        SnippetFormat::Hyprland => "hyprland",
        SnippetFormat::Niri => "niri",
        SnippetFormat::River => "river",
        SnippetFormat::Labwc => "labwc",
        SnippetFormat::Wayfire => "wayfire",
        SnippetFormat::Cosmic => "cosmic",
        SnippetFormat::Kde => "kde",
        SnippetFormat::Gnome => "gnome",
        SnippetFormat::Generic => "generic",
    }
}

#[test]
fn snippets_per_compositor() {
    for format in SnippetFormat::ALL {
        let rendered = [DOC_EXE, "quick-translate", SPACED_EXE]
            .map(|exe| generate(format, &ToggleCommand::new(exe)).render())
            .join("---\n");
        insta::assert_snapshot!(snapshot_name(format), rendered);
    }
}

#[test]
fn compositor_maps_to_format() {
    let cases = [
        (Compositor::Sway, SnippetFormat::Sway),
        (Compositor::Hyprland, SnippetFormat::Hyprland),
        (Compositor::Niri, SnippetFormat::Niri),
        (Compositor::River, SnippetFormat::River),
        (Compositor::Labwc, SnippetFormat::Labwc),
        (Compositor::Wayfire, SnippetFormat::Wayfire),
        (Compositor::Cosmic, SnippetFormat::Cosmic),
        (Compositor::Kde, SnippetFormat::Kde),
        (Compositor::Gnome, SnippetFormat::Gnome),
        (
            Compositor::Other(Some("Weird".into())),
            SnippetFormat::Generic,
        ),
        (Compositor::Unknown, SnippetFormat::Generic),
    ];
    let cmd = ToggleCommand::new(DOC_EXE);
    for (compositor, format) in cases {
        let snippet = snippet_for(&compositor, &cmd);
        assert_eq!(snippet.format, format, "{compositor:?}");
        assert!(
            snippet.text.contains(DOC_EXE),
            "{format:?} must invoke the AppImage path"
        );
        assert!(snippet.text.contains("--toggle"));
    }
}

#[test]
fn appimage_path_is_used_from_env() {
    let cmd = ToggleCommand::detect_from(
        Some(DOC_EXE.into()),
        None,
        Some("/tmp/.mount_QuickTr/usr/bin/quick-translate".into()),
    );
    let snippet = snippet_for(&Compositor::Niri, &cmd);
    assert_eq!(
        snippet.text,
        format!("Ctrl+Alt+C {{ spawn \"{DOC_EXE}\" \"--toggle\"; }}")
    );
}
