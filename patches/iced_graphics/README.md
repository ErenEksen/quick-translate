# iced_graphics 0.14.0 (patched)

The published `iced_graphics` 0.14.0 crate (MIT, © the iced contributors,
https://github.com/iced-rs/iced) with one change in `src/text.rs`: `Text::visible_bounds` for
cached text accounts for its alignment.

The renderer draws vertically centered text (every `pick_list` label) half its height above its
anchor point, but the damage rectangle started at the anchor. On surfaces that redraw only the
damaged region (the layer-shell popup, through `patches/softbuffer`), a label that got shorter
without anything else changing around it — "Auto-detect (English)" becoming "Auto" after the
DeepL/AI switch — kept the top half of its old glyphs on screen.

`fonts/FiraSans-Regular.ttf` is left out: it is only used by the `fira-sans` feature, which this
workspace does not enable. Drop this copy once iced ships the fix.
