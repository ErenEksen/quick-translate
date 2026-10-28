# Spike 2.2: iced_layershell capabilities

Status: **GO for iced_layershell 0.19.1** (D4). Everything D3 needs works. Two pitfalls must be handled in task 8.3: the keepalive surface and focus events. libcosmic is not needed. Its evaluation is kept at the end for reference.

Date: 2026-10-22. Test binary: `spikes/overlay-probe` (iced 0.14.0, iced_layershell 0.19.1, layershellev 0.19.1, wgpu 27.0.1). The test environments are the same as in [overlay-probe.md](overlay-probe.md): nested KWin 6.8.0 (same binary as the live session), headless Sway 1.12 and nested niri 26.04. The live KWin session was re-checked on 2026-10-23, with the session unlocked. See the "Live KWin" row; the raw log is `spikes/logs/live-kwin-2026-10-23.log`.

## Results

| Capability | KWin 6.8 (nested) | Sway 1.12 (headless) | niri 26.04 (nested) | How |
|---|---|---|---|---|
| Daemon with no surface at start | ✅ | ✅ | ✅ | `iced_layershell::daemon(..)` with `LayerShellSettings { start_mode: StartMode::Background, .. }`. `size` is not needed in Background mode. |
| Create a layer surface per output at runtime | ✅ 1 and 2 outputs; 2nd at scale 1.5 | ✅ 2 outputs, 2nd at 1.5 | ✅ | `Message::layershell_open(NewLayerShellSettings { output_option: OutputOption::OutputName(name), .. })` |
| Destroy at runtime (losers right away, all on hide) | ✅ Closed about 4 ms after `RemoveWindow` | ✅ | ✅ | `Message::RemoveWindow(id)`; a `window::Event::Closed` follows |
| Repeated show/hide (`--cycles 3`) | ✅ | n/a | ✅ | Ids are new every time; nothing leaks between cycles |
| Exclusive keyboard, focus right after map | ✅ The first key (fake Esc) went to the overlay, which then exited | ✅ Key `a` went to the winner | ✅ (raw) | `keyboard_interactivity: KeyboardInteractivity::Exclusive`. With several overlays, the compositor focuses the **last mapped** one; after `RemoveWindow` on the losers it moves to the winner (logged on KWin and Sway). |
| Switch the winner to exclusive later | ✅ (raw probe: keyboard enter 1.3 ms after the change) | n/a | n/a | `Message::KeyboardInteractivityChange { id, keyboard_interactivity: Exclusive }`. Optional: create overlays with `None` so no loser ever takes focus. |
| Input region | ✅ After `SetInputRegion` to the panel rect, `CursorLeft` fired when the pointer left the panel and the click fell through | ✅ Same | n/a | `Message::SetInputRegion { id, callback: ActionCallback::new(move \|r\| r.add(x, y, w, h)) }` |
| Outside-click detection, full input region (D3 default) | ✅ `ButtonPressed` outside the panel reached the probe widget, which hid the popup | ✅ | n/a | Don't set an input region. Hit-test the panel rect in `update`. |
| Pointer position on enter | ✅ +12 to +22 ms from the request (with keepalive) | ✅ only with the `swaymsg seat - cursor move 0 0` rebase (see overlay-probe.md) | ✅ | See "Reading the pointer position" |
| Fractional scale / logical coordinates | ✅ The 1.5 output gives a logical surface of 1920x1080, positions in logical px, clamping correct | ✅ | n/a | Nothing extra to do |
| CLIPBOARD survives popup close | ❌ **without keepalive**: polled `wl-paste` saw the value at 652 ms; it was gone 70 ms after the surface closed. ✅ **with keepalive**: the value was still there after close. | not tested | not tested | See pitfall 1 |
| Live KWin 6.8 (2026-10-23) | ✅ The daemon started with the keepalive surface. Each overlay was created on eDP-1 and destroyed again over 3 cycles, with no leak. The overlay opened +14 to +20 ms after the request. Keyboard enter arrived right on map (raw probe: 2.2 ms), along with the pointer position. Not run live: the Esc key and leaving the input region, which need a person; nested KWin covers them. The `--copy-test` run is **invalid**: the screen auto-locked during it, and no enter or focus arrived. | | | |

## API to use in the real layer shell (task 8.3)

```rust
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer, NewLayerShellSettings, OutputOption};
use iced_layershell::settings::{LayerShellSettings, Settings, StartMode};
use iced_layershell::actions::ActionCallback;

#[iced_layershell::to_layer_message(multi)]   // adds NewLayerShell, RemoveWindow, SetInputRegion{id,callback},
#[derive(Debug, Clone)]                       // KeyboardInteractivityChange{id,..}, MarginChange{id,..}, ... and
enum Message { /* app messages */ }           // Message::layershell_open(settings) -> (window::Id, Task<Message>)

iced_layershell::daemon(boot, || "quick-translate".into(), update, view)
    .style(|_, theme| iced::theme::Style { background_color: iced::Color::TRANSPARENT, text_color: theme.palette().text })
    .subscription(subscription)
    .settings(Settings {
        layer_settings: LayerShellSettings { start_mode: StartMode::Background, ..Default::default() },
        ..Default::default()
    })
    .run()

// one overlay per output
NewLayerShellSettings {
    size: None,
    layer: Layer::Overlay,
    anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
    exclusive_zone: Some(-1),
    margin: None,
    keyboard_interactivity: KeyboardInteractivity::Exclusive,
    output_option: OutputOption::OutputName(name),   // name = wl_output v4 `name` / xdg_output name
    events_transparent: false,
    namespace: Some("quick-translate".into()),
}

// keepalive (create once in boot and never remove; see pitfall 1)
NewLayerShellSettings {
    size: Some((1, 1)), layer: Layer::Background, anchor: Anchor::Top | Anchor::Left,
    exclusive_zone: Some(0), margin: None, keyboard_interactivity: KeyboardInteractivity::None,
    output_option: OutputOption::Active, events_transparent: true, namespace: Some("quick-translate-keepalive".into()),
}
```

Reference implementation: `spikes/overlay-probe/src/main.rs`. See `boot`, `update` (`Show`, `Probe`, `Hide`), `pick_winner` and the `Probe` widget.

### Enumerating outputs

iced_layershell does **not** pass output add/remove events or names to the app, although layershellev tracks them internally in `xdg_info_cache`. The spike lists the names on a short-lived side connection (`list_outputs()`: `wl_output` v4 `name`, one roundtrip, about 4 ms). The real daemon should either do this on every Show, or keep its own registry listener on the platform probe connection (task 5.1), and pass `OutputOption::OutputName`.

Pitfall: an **unknown name silently becomes "compositor chooses"** (`OutputOption::OutputName` resolves to `None` in layershellev `lib.rs:2617`). It does not fail.

Alternative: `Settings::with_connection` shares one `wayland_client::Connection`, which allows `OutputOption::Output(WlOutput)`.

### Reading the pointer position

- `mouse::Event::CursorEntered` has **no position** in iced, and iced_layershell turns `wl_pointer.enter` into exactly that (`conversion.rs:44`). Before dispatching, however, iced_layershell stores the enter position in the window state (`multi_window/state.rs:163`). A widget's `update` therefore gets `cursor.position() == Some(enter_pos)` on `CursorEntered`.
- The spike's `Probe` widget is a full-surface leaf widget placed under the panel in a `stack!`. It publishes the position on `CursorEntered`, `CursorMoved` and `ButtonPressed`.
- A subscription (`event::listen_with`) sees `CursorEntered` but cannot get the position.

## Pitfalls (must handle)

1. **Removing the last surface destroys the renderer and the clipboard.** In `multi_window.rs` `handle_closed_event`, `window_manager.is_empty()` leads to `remove_compositor()`, which sets `compositor = None` and `clipboard = unconnected()`. Consequences:
   - The next show rebuilds wgpu, taking **345 to 770 ms** (release, NVIDIA) instead of 12 to 22 ms. That breaks the 100 ms budget.
   - **CLIPBOARD set with `iced::clipboard::write` is lost** as soon as the popup closes, because the smithay-clipboard worker is dropped. That breaks the "Paste after close" scenario.
   - **Fix (verified): keep a 1x1 transparent Background-layer surface with `events_transparent: true` and keyboard `None` for the whole daemon lifetime.** It costs about 450 ms once at daemon start (renderer init) and nothing per popup.
   - Notes: the keepalive also gets `Unfocused` events (see pitfall 2), so ignore it. Its Opened size on a 1.5-scale output was 1.333, which is a harmless rounding quirk.
2. **`window::Event::Focused` and `Unfocused` don't reliably mean keyboard focus.**
   - layershellev emits `Focused` from `update_current_surface`. That function runs when a surface is created (`push_window`), on pointer button and touch, and on `wl_keyboard.enter` only if the surface changed. As a result the first `Focused` for an overlay is often missing or early.
   - `Unfocused` on `wl_keyboard.leave` goes to the *current surface* id. The spike saw `Unfocused` for the keepalive surface after an overlay was removed.
   - So don't build "close on focus loss" on these events. With `exclusive` on the overlay layer, focus loss is rare anyway. Use outside-click and Esc, and treat `Unfocused` on the *winner's* id only as a hint.
3. **Arm the 50 ms placement fallback on `window::Event::Opened`** (or the first `RedrawRequested` from `event::listen_raw`), not when `NewLayerShell` is requested. A cold renderer takes hundreds of ms to show the surface. See overlay-probe.md for allowing a late `enter` to reposition the panel.
4. `SetInputRegion` uses **one `wl_region` shared by all windows**. Each call subtracts that window's rect and then runs your callback (`multi_window.rs:746`). That is fine for one region per popup. Don't rely on regions for several windows of different sizes at once. D3 itself should **not** set an input region, because outside-click detection needs the whole surface.
5. Each `NewLayerShell` binds a new `zwlr_layer_shell_v1` global object (`layershellev lib.rs:2629`). This is a small leak per surface. A daemon that creates N surfaces per popup leaks N small protocol objects per popup. It's acceptable, but worth an upstream issue.
6. Don't hard-exit with `std::process::exit` from another thread while wgpu is running, because it dumped core on NVIDIA. Exit with `iced::exit()`. (The spikes use the thread only as a backstop.)
7. IME cursor rectangles are not scaled by the app-level `scale_factor()` (see ime.md). Keep `scale_factor` at 1.
8. iced_layershell 0.19.1 needs winit-core and winit-common pinned to `0.31.0-beta.2` (`cargo update -p winit-core --precise 0.31.0-beta.2 && cargo update -p winit-common --precise 0.31.0-beta.2`), as in the root `Cargo.lock`.

## Live KWin procedure (create/destroy and focus done on 2026-10-23; repeat the Esc, input-region and copy checks at an unlocked screen)

Check that the session is unlocked first:

```sh
busctl --user call org.freedesktop.ScreenSaver /ScreenSaver org.freedesktop.ScreenSaver GetActive   # must print: b false
kscreen-doctor --dpms show                                                                          # must be on
```

Then, with the mouse still:

```sh
cd /home/axis/Applications/quick-translate
timeout 8 target/agent-spikes/release/overlay-probe --ms 3000 --keepalive --cycles 3                 # dynamic create/destroy, per-cycle timings
timeout 8 target/agent-spikes/release/overlay-probe --ms 3000 --keepalive                            # press Esc within 3 s: expect "first key Named(Escape)"
timeout 8 target/agent-spikes/release/overlay-probe --ms 3000 --keepalive --input-region             # move the mouse off the panel: expect "CursorLeft"; a click there reaches the app below
timeout 8 target/agent-spikes/release/overlay-probe --ms 3000 --keepalive --copy-test & sleep 1.5; wl-paste -n   # expect qt-probe-clipboard-OK
```

Sway (real session): run the same commands with `--sway-rebase` added.

## libcosmic evaluation (fallback, not needed)

This was a desk review of libcosmic master @430f3d4 (2026-10-19) and pop-os/iced @229186b. Nothing was built.

- **It is not on crates.io.** It's git-only: it depends on git `cctk` (cosmic-protocols rev `c0cff4d`), `cosmic-settings-daemon`, and an `iced` path submodule. The iced fork is based on **iced 0.14.0** and also pulls in forks of winit, softbuffer, window_clipboard and cryoglyph. Upstream `cosmic-client-toolkit` 0.2.0 is on crates.io, but libcosmic doesn't use it.
- **Layer API.** `cosmic::iced::platform_specific::shell::commands::layer_surface::{get_layer_surface, destroy_layer_surface, set_input_zone, set_keyboard_interactivity, ..}` with `SctkLayerSurfaceSettings { output: IcedOutput::Output(WlOutput), input_zone, .. }`.
- **Outputs** come as `event::wayland::Event::Output(OutputEvent::Created/Removed, WlOutput)`, which is better than iced_layershell.
- **Pointer enter** sends `CursorEntered` followed by `CursorMoved{position}`.
- Layer surfaces and xdg windows can live in one process.
- **IME on layer surfaces works only behind the `single-instance` feature.** `text_input.rs:72` drops all text-input events otherwise. That feature pulls in zbus and D-Bus activation.
- MPL-2.0 licence, Rust 1.93 or newer. The build is heavy (cosmic-config, cosmic-theme, i18n, a11y). It uses COSMIC theming and fonts, so an AppImage has to bundle Open Sans and Noto Sans Mono or override them.
- Verdict: libcosmic is a viable fallback, but there is no reason to switch. iced_layershell meets every D3 requirement once pitfalls 1 to 3 are handled.
