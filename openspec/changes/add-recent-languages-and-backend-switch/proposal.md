# Proposal

## Why

Users who translate between a few languages scroll through 37 targets every time they change
language. Users who set up both DeepL and an AI model must open settings to compare them or to
fall back when one fails (DeepL quota, a local model that is off). And at the popup's minimum
width, or with a long model name, the header pushed the Copy and settings buttons out of the
panel, which broke the requirement that the minimum size keeps every header control usable.

## What Changes

- Both language selectors list the three languages the user picked most recently above the full
  list, separated by a divider. The lists are remembered in `config.toml`
  (`recent-targets`, `recent-sources`).
- When DeepL and an OpenAI-compatible endpoint are both set up, the backend indicator in the
  header becomes a small switch. Flipping it translates the current text again with the other
  backend and saves the choice as `backend`, like **Settings → Backend**.
- The header moves its right-hand controls to a second line when they do not fit next to the
  language controls, instead of clipping them. The "Translating…" text is shown only where it
  has room, so it never moves other controls.
- The vendored `iced_graphics` copy fixes the damage rectangle of aligned text, which left half
  of a shortened selector label on screen in the layer-shell popup.

## Capabilities

### Modified Capabilities

- `translation-popup`: recent languages in the selectors, the DeepL/AI switch, and a header
  that never hides controls.

## Impact

- `quick-translate-core`: `Config::recent_targets`/`recent_sources`, `remember_target`,
  `remember_source`, `OpenAiConfig::is_set_up`; `language::push_recent`.
- `quick-translate-ui`: popup state, effects and view; new `popup/header.rs` layout widget;
  `ShellEvent::RememberSource` and `ShellEvent::BackendSwitched`; `ShowRequest` carries the
  second translator.
- `quick-translate` daemon: builds both backends when both are set up, swaps them on a switch
  and persists the choice and the recent languages.
- `patches/iced_graphics` and `[patch.crates-io]` in `Cargo.toml`.
