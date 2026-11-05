# Tasks

## 1. Core

- [x] 1.1 Add `recent-targets` and `recent-sources` to the config (omitted when empty), `Config::remember_target`, `Config::remember_source`, sanitized `recent_targets()`/`recent_sources()`, `language::push_recent` and `OpenAiConfig::is_set_up`. Verify with unit tests for the most-recently-used order, the cap of three, normalization, hand-edited lists and TOML round-trips that keep unknown keys.

## 2. Popup

- [x] 2.1 Keep the recent lists in the popup, list them above the full lists with a divider, and emit `PersistTarget`/`RememberSource` when a choice is committed. Verify with popup tests for mouse picks, keyboard browsing, swap, the divider and reset.
- [x] 2.2 Add the DeepL/AI switch (`Control::Backend`, `Message::BackendToggled`, `Effect::SwitchBackend` before the new `StartTranslation`). Verify with popup tests for re-translation, cancelling the old stream, pending capture, failures, the Tab order and keyboard activation, and host tests that the translator is swapped and the daemon told.
- [x] 2.3 Add the measured header layout (`popup/header.rs`). Verify the arrangement with unit tests and with screenshots at 480, 600, 650 and 720 px in headless Sway.

## 3. Daemon

- [x] 3.1 Build the second backend when both are set up, pass it with each popup, swap it on `BackendSwitched` and persist the backend and the recent languages. Verify with backend-build tests for every combination of keys and endpoints, `Popups` tests and a config-writer test.
- [x] 3.2 Verify end to end in headless Sway with a private session bus, a DeepL session key and a local mock OpenAI-compatible server: the switch re-translates, `config.toml` gets `backend = "openai"`, the choice and the recent target survive closing and reopening the popup, in the layer shell and in the window shell.

## 4. Rendering

- [x] 4.1 Vendor `iced_graphics` 0.14.0 with alignment-aware damage bounds for cached text. Verify the label no longer leaves stale glyphs after the switch in the layer-shell popup, and with a unit test in the vendored crate.

## 5. Manual

- [ ] 5.1 Run X31–X33 from `docs/testing.md` on KDE Plasma and GNOME with a real DeepL key.
