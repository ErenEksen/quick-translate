# Spike 2.3: IME in iced 0.14 `text_editor` (layer surface and winit window)

Status: **partially verified.** The text-input-v3 plumbing and Turkish XKB input are verified in both shells. **Composition with a real IME (fcitx5 or ibus, Japanese) is NOT TESTED**, because neither is installed on the dev machine. The procedure below is ready to run. Based on the source review, nothing blocks D4, but there are three caveats to watch in the manual test.

Date: 2026-10-22. Binary: `spikes/ime-probe` (iced 0.14.0, iced_widget 0.14.2, iced_winit 0.14.1 on winit 0.30.13, iced_layershell 0.19.1, layershellev 0.19.1).

## Method

`ime-probe` shows one `text_editor`, focused on start, in one of two shells:
- `--layer`: an iced_layershell `application`. This is an overlay layer surface of 800x300 with keyboard `Exclusive`, the same shell type as the D3 popup.
- `--window`: `iced::application`, a winit xdg_toplevel, which is the D4 window shell.

It logs every `input_method::Event` (Opened, Preedit, Commit, Closed) from the event stream, and every text change of the editor. Esc exits. A hard timer caps the run at 3 s, or at 120 s with `--manual` for a human tester.

What was run (nested KWin 6.8 `--virtual`, which has `zwp_text_input_manager_v3` v2 but no input-method daemon):
- `WAYLAND_DEBUG=1` to capture the text-input-v3 requests;
- Turkish keys typed with KWin fake-input (`ğ ü ş ı i c` on the `tr` XKB layout, then Esc).

## Results

| Check | `--layer` (iced_layershell) | `--window` (winit) |
|---|---|---|
| Binds `zwp_text_input_manager_v3` and creates a `zwp_text_input_v3` per seat | ✅ | ✅ |
| After `text_input.enter(surface)` on keyboard focus: `enable`, `set_content_type(none, normal)`, `set_cursor_rectangle(17,46,1,21)` (surface-local logical px at the caret), `commit` | ✅ | ✅ |
| Cursor rectangle follows the caret while typing (x = 27, 34, 38, 43 ...) | ✅ | ✅ |
| `input_method::Event::Opened` delivered to the app | ❌ **never.** layershellev enables only if `ime_allowed` was already set when `enter` arrived, so `Ime::Enabled` is never emitted. Harmless for `text_editor`: Preedit and Commit are handled without Opened (`iced_widget text_editor.rs:1340`). | ✅ |
| Turkish via XKB `tr` (`ğüşıic`) committed exactly as typed | ✅ (2 of 2 runs without WAYLAND_DEBUG; with WAYLAND_DEBUG the first key came before the surface existed) | ✅ |
| **fcitx5 + mozc, Japanese composition** (preedit shown, single commit of 日本語) | **not tested (not available on dev machine)** | **not tested** |
| **ibus + anthy (or mozc), Japanese** | **not tested** | **not tested** |
| **fcitx5 / ibus Turkish layout** | **not tested** | **not tested** |
| Sway, niri, Hyprland, GNOME with an IME | **not tested** | **not tested** |

Logs: `spikes/logs/ime-kwin-nested.log`.

## What the source says

**iced 0.14 widgets.**
- `text_editor` asks for IME on redraw while focused: `InputMethod::Enabled { cursor, purpose: Normal, preedit }` (`iced_widget-0.14.2/src/text_editor.rs:339-378`, sent at `:927`). It handles `Preedit` and `Commit` (`:779-802`). A commit becomes `Edit::Paste`, so it runs through our `on_action`, and the debounce logic sees it as a normal edit.
- The preedit text is **not drawn inline**. The runtime draws it as an overlay just below the caret: iced_winit `window.rs:263-275`, iced_layershell `ime_preedit.rs:43`.

**winit 0.30 (window shell).**
- Uses text-input-v3 only. Preedit and commit are buffered until `done`, an empty Preedit is sent before each Commit, and the `done` serial is ignored.
- `delete_surrounding_text` is not handled (`winit text_input/mod.rs:152`).
- IME is enabled on demand through `set_ime_allowed` (iced_winit `window.rs:277-304`).

**iced_layershell / layershellev (layer shell).**
- Has its own text-input-v3 client (layershellev `lib.rs:1168-1260` and `:1968-2068`), used by both `application` and `daemon` (iced_layershell `multi_window.rs:1000-1054`).
- Its commit and preedit logic matches winit's. `delete_surrounding_text` is ignored, and nothing ever sends `set_surrounding_text`, so reconversion and delete-surrounding features are unavailable in both shells.
- It does not implement text-input v1 or v2. The `zwp_input_method`/`input_panel` and `virtual_keyboard` imports exist for writing on-screen keyboards, not for being an IME client.

**Caveats to watch** (from static review; not observed yet):
1. **The cursor rectangle is not divided by the app-level `scale_factor()`** (iced_layershell `multi_window.rs:1046`, same in iced_winit `window.rs:287`). With `scale_factor` 1, which is what we use, the values are correct. Fractional *output* scale doesn't matter, because the protocol uses surface-local logical coordinates. **Never set a custom `scale_factor` on the popup**, or the candidate window will be misplaced.
2. **The layer shell has a single `ime_allowed` flag for the whole app.** When a window that had IME goes back to Disabled, it can turn IME off for the window that just got focus (`multi_window.rs:1034` vs `window_manager.rs:31`). This doesn't matter for D3, because only the winner overlay contains a `text_editor` and the losers never request IME. Avoid putting editors in two layer surfaces at once.
3. **After a text-input leave/enter cycle, the cursor rectangle isn't sent again until the caret moves.** The protocol says `enable` resets it. The candidate window may then appear at a default position the first time. Look for this in the manual test.
4. layershellev destroys the seat's text_input whenever any seat capability is removed, and recreates it only when a keyboard is re-added (`seat.rs:150-180`). Unplugging a mouse could therefore break IME until the keyboard reconnects. This is an edge case.

**Conclusion.** Unless the manual test shows otherwise, IME in the layer-surface popup should behave like IME in the winit window. GTK4 or libcosmic is not needed for IME (and libcosmic's layer-surface IME is gated behind its `single-instance` feature; see iced-layershell.md). The **Verify** step of 2.3 (composed text committed correctly with fcitx5 and ibus) stays **open** until the manual procedure below is run.

## Manual procedure (fcitx5 / ibus)

Build:

```sh
cd /home/axis/Applications/quick-translate
CARGO_TARGET_DIR=$PWD/target/agent-spikes cargo build -j 8 --release --manifest-path spikes/ime-probe/Cargo.toml
```

Session must be unlocked:

```sh
busctl --user call org.freedesktop.ScreenSaver /ScreenSaver org.freedesktop.ScreenSaver GetActive   # must print: b false
```

### fcitx5 on KDE Plasma 6

1. Install `fcitx5 fcitx5-mozc fcitx5-configtool fcitx5-qt fcitx5-gtk`.
2. In System Settings > Keyboard > Virtual Keyboard, choose **Fcitx 5**. KWin then starts fcitx5 over `zwp_input_method_v1` and serves text-input-v3 to clients. Don't set `GTK_IM_MODULE` or `QT_IM_MODULE`.
3. In fcitx5-configtool, add **Mozc** and **Keyboard - Turkish**.
4. Run each shell:

```sh
target/agent-spikes/release/ime-probe --layer  --manual --ms 90000
target/agent-spikes/release/ime-probe --window --manual --ms 90000
```

The layer variant grabs the keyboard until Esc or 90 s.

5. Switch to Mozc (Ctrl+Space), type `nihongo`, press Space, then Enter.
   - **Pass:** while typing, the log shows `input_method event: Preedit("にほんご", Some(..))` and the preedit is visible just below the caret. The candidate window appears next to the caret. After Enter, the log shows `Preedit("", None)`, then `Commit("日本語")` and **one** `text now: "日本語"`. There is no romaji, no duplicated text, and no lost characters.
6. Switch to Turkish and type `ğüşıöçİ`. **Pass:** the log shows `text now: "ğüşıöçİ"`.
7. Repeat steps 5 and 6 in the window variant. In `--window`, `Opened` appears first. In `--layer` it does not, which is expected.

### ibus

- **On KDE:** install `ibus ibus-anthy` (or `ibus-mozc`) and choose **IBus Wayland** under Virtual Keyboard. Run the same steps with the Anthy engine.
- **On GNOME 48+:** ibus is built into Mutter. Only `--window` applies there, since GNOME has no layer-shell.

### Sway, Hyprland, niri

Run `fcitx5 -d --replace` in the session. The compositor must support `zwp_input_method_v2`, which Sway 1.12, Hyprland and niri do. Then run both variants as above. Record any compositor where the candidate window is misplaced or a commit is lost.

Record the results in the table above (shell × IME × compositor).
