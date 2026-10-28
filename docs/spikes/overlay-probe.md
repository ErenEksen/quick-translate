# Spike 2.1: D3 overlay host (pointer enter without motion)

Status: **GO for D3**, with the conditions listed at the end. Fallback B does not fix the one problem found (Sway), so B is not recommended.

Date: 2026-10-22. Data comes from nested or headless test compositors. The live KWin rows were measured on 2026-10-23 on the unlocked session (one 1920x1200 output, eDP-1, scale 1). The raw log is `spikes/logs/live-kwin-2026-10-23.log`.

## Question

D3 creates one transparent overlay layer surface per output: layer `overlay`, anchored on all four edges, exclusive zone -1, keyboard `exclusive`. The surface that gets the first `wl_pointer.enter` or `motion` wins, and the panel is drawn at the pointer position plus 12 px, clamped to the output.

The spike answers two questions per compositor:
- Does `enter` arrive with a position as soon as the surface maps, while the pointer is not moving?
- How long does the first event take?

## Method

Two spike binaries were used:

| Binary | Path | What it measures |
|---|---|---|
| `wl-raw-probe` | `spikes/wl-raw-probe/src/main.rs` | Protocol level, no toolkit. One layer surface per output (overlay, all anchors, exclusive zone -1, keyboard exclusive). Each surface gets a 1x1 transparent `wl_shm` buffer stretched with `wp_viewporter`. This is exactly fallback B's probe surface. `--full-buffer` attaches a full-size buffer instead. The probe logs these times, in ms since the surfaces were created: `configure`, buffer commit, first frame callback, `wl_surface.enter(output)`, `wl_keyboard.enter`, `wl_pointer.enter` (surface-local position and output), and the first `motion`. Options: `--destroy-losers` destroys the other surfaces on the first enter. `--kb-late` creates the surfaces with keyboard `none` and switches the winner to `exclusive`. |
| `overlay-probe` | `spikes/overlay-probe/src/main.rs` | The real D3 shape on iced 0.14 and iced_layershell 0.19.1. It runs in daemon mode. On each Show it creates one overlay per output with `NewLayerShell` and `OutputOption::OutputName`. A full-surface probe widget reports `cursor.position()` on `CursorEntered`, `CursorMoved` and `ButtonPressed`. The first surface with a position wins, the losers get `RemoveWindow`, and the panel (720x320) is drawn at pointer + 12, clamped. If no position arrives, a 50 ms fallback centers the panel. Options: `--keepalive`, `--cycles N`, `--input-region`, `--kb-late`, `--sway-rebase`, `--copy-test` (see spike 2.2). |
| `fake-pointer` | `spikes/wl-raw-probe/src/bin/fake-pointer.rs` | Test harness for nested compositors only. It refuses to run unless `WAYLAND_DISPLAY=qt-*`. It creates a pointer device with `org_kde_kwin_fake_input` (KWin) or `zwlr_virtual_pointer_v1` (wlroots, niri), plus an optional `zwp_virtual_keyboard_v1` (`QT_KEYMAP=`). It warps the pointer to X,Y once and then **stays idle**. That idle state is the real scenario: the pointer is at rest when the hotkey fires. It can also run a script of later moves, clicks and keys (`MS:move:X,Y;MS:click;MS:key:CODE`). |

Every binary exits by itself through a hard timer (`--ms`). Runs were also wrapped in `timeout`. Raw logs are in `spikes/logs/`.

### Environments

| Name | How it was run | Renderer |
|---|---|---|
| KWin 6.8.0, nested | The same `kwin_wayland` binary as the live session, run as `kwin_wayland --virtual --no-lockscreen --no-global-shortcuts --output-count N` in `dbus-run-session`, with a private `XDG_CONFIG_HOME`. The second output was set to scale 1.5 through the private `kwinoutputconfig.json`. | NVIDIA RTX 5080 (host GPU); iced uses wgpu/Vulkan |
| Sway 1.12, headless | Arch container (podman, rootless): `WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman sway`. A second output was added with `swaymsg create_output`, with `output HEADLESS-2 position 1920 0 scale 1.5`. | pixman; iced on Mesa software rendering |
| niri 26.04, nested | Nested inside the headless sway (winit backend, `LIBGL_ALWAYS_SOFTWARE=1`). Its pointer is niri's own `zwlr_virtual_pointer_v1` device, so the pointer logic is niri's own. | llvmpipe |
| cosmic-comp 1.10.0, nested | Nested inside the headless sway (winit backend). It has no virtual-pointer global, so its pointer never moved from (0,0). | llvmpipe |
| Hyprland 0.56.2 | **Not tested.** Nested Hyprland would not start in the container ("CBackend::create() failed!" because there is no DRM device). | n/a |
| GNOME | Out of scope: no layer-shell. | n/a |

Versions: iced 0.14.0, iced_widget 0.14.2, iced_layershell 0.19.1, layershellev 0.19.1, wgpu 27.0.1, wayland-client 0.31.15, rustc 1.99.1. The spike lockfiles pin winit-core and winit-common to 0.31.0-beta.2, as the root lock does.

## Results: protocol level (`wl-raw-probe`)

The pointer was warped once before the probe started and did not move afterwards. Times are ms since the layer surfaces were created.

| Compositor | `enter` with no motion? | Time to first pointer event | Keyboard enter on map | First frame callback | Notes |
|---|---|---|---|---|---|
| **KWin 6.8 (nested)**, 1 output | **Yes**, with exact pos (500, 400) | **1.4 ms** (1x1 + viewporter), 1.0 ms (full buffer) | yes, 1.4 ms | 2.4 ms / 19 ms | `enter` comes right after the buffer commit, before the frame callback. |
| KWin 6.8 (nested), 2 outputs (2nd at scale 1.5) | **Yes**, on the correct output, with logical surface-local pos (1900, 1060) on the 1920x1080 logical surface | 1.6 to 2.7 ms | Goes to the **last mapped** surface. With `--destroy-losers` it moves to the winner within 0.1 ms. | 3 to 55 ms | `--kb-late` (all surfaces created with keyboard `none`, winner switched to `exclusive`) also works: keyboard enter on the winner at 2.9 ms. |
| **Sway 1.12 (headless)** | **No.** No `enter` at all until the pointer moves. On the first motion, `enter` arrives with the new position, followed by `motion`. | none without motion | yes, 1.0 ms | 1 to 8 ms | **Workaround verified:** `swaymsg seat - cursor move 0 0` (a zero relative move over IPC) makes Sway rebase the cursor. It sends `enter` with the correct position, with no motion event, within 1 ms. This also works on 2 outputs with scale 1.5: `enter` on HEADLESS-2 at (580, 300). |
| **niri 26.04 (nested)** | **Yes**, pos (498.4, 385.2) | 107 ms (1x1), 142 ms (full buffer) | yes, same instant as `enter` | same instant | niri delivers focus and `enter` after its first composited frame. The absolute times are inflated by software rendering in the nested setup and are indicative only. |
| **cosmic-comp 1.10 (nested)** | **Yes** (pos (0,0), because the nested pointer never moved) | 89 to 272 ms | yes, 45 ms | 180 ms | Indicative only. |
| Hyprland | not tested (not available on the dev machine) | | | | See procedure below. |
| **Live KWin 6.8 (wayland-1)**, 2026-10-23, unlocked | **Yes**, exact pos (745.3, 692.6) on eDP-1, `no motion before it: true` | **2.2 ms** (1x1 + viewporter and full buffer) | yes, 2.2 ms | 4.2 ms / 18 ms | Matches the nested KWin. An earlier run on 2026-10-22 was invalid: the session was locked (`LockedHint=yes`, DPMS off) and got no frame callbacks or enter events. |

## Results: iced_layershell overlay (`overlay-probe`, release build)

The `+ms` figures are measured from the `NewLayerShell` request.

| Compositor | Without keepalive | With `--keepalive` (1x1 surface kept alive) | Placement |
|---|---|---|---|
| KWin 6.8 (nested, NVIDIA) | First view after +478 ms, pointer position after +492 ms. This is the wgpu compositor being created. `--cycles 3` show times were 766, 362 and 345 ms, because iced_layershell destroys the renderer every time the last surface closes. | Opened after +12 to +18 ms. **Pointer position (via `CursorEntered` + `cursor.position()`) after +12 to +22 ms.** | (500,400) gives the panel at (512,412). On 2 outputs the winner is the correct output (Virtual-1), the loser is removed and Closed by +26 ms, and keyboard focus moves to the winner. With the pointer at (1900,1060) on a 1.5-scale output the panel is clamped to (1200,760) on the 1920x1080 logical surface. |
| Sway 1.12 (headless) | n/a | Opened after +24 ms. With nothing else, the **50 ms fallback fires** and the panel is centered on the first surface, which is not necessarily the output with the pointer. With `--sway-rebase` (spawns `swaymsg seat - cursor move 0 0` on Opened), the pointer position arrives at +32 ms on the correct output (HEADLESS-2, scale 1.5) and the panel is clamped to (560,312). | |
| niri 26.04 (nested, software) | n/a | Cycle 1: Opened after +193 ms (software rendering), and the fallback fired before `enter`, because niri sends `enter` after its first frame. Cycle 2: position at Opened + 9 ms. | Correct |
| Live KWin 6.8 (2026-10-23, iced default wgpu on NVIDIA) | n/a (not run) | The keepalive surface opened after +428 to +481 ms, which is the one-time wgpu start-up. Then each overlay opened after +14 to +20 ms, with the **pointer position via `CursorEntered` after +16 to +20 ms**. Across `--cycles 3`, the per-cycle show-to-position times were 20.3, 17.3 and 15.7 ms. Overlays closed about 4 ms after `RemoveWindow`. | Correct: the panel is at pointer + 12, (757, 704). |

Outside-click dismissal (KWin nested, 2 outputs): a scripted click at a spot outside the panel was delivered as `ButtonPressed` to the probe widget, which hid the popup. With `--input-region` limited to the panel, the pointer leaving the panel produced `CursorLeft`, and the click fell through to whatever was below (see 2.2).

## Conclusions

1. **KWin, niri and cosmic-comp send `wl_pointer.enter` with the position when a surface maps, without motion.** On KWin it takes about 1 to 2 ms after the first buffer commit. **Sway does not**: it sends `enter` only on the next pointer motion. Hyprland is not measured.
2. **Fallback B does not help with this.** B's probe is a full-output surface with a 1x1 buffer stretched by the viewport, which is exactly what `wl-raw-probe` measured. It has the same enter semantics as D3's overlay (the 1x1 and full-buffer rows match). The only thing B saves is GPU memory while the popup is visible.
3. **Sway mitigation:** `swaymsg seat - cursor move 0 0` (Sway IPC, `$SWAYSOCK`) is a zero-length relative move. It makes Sway send `enter` with the correct position and no motion, in about 1 ms. It doesn't move the cursor or type into any application, but it is still a synthetic input request sent through compositor IPC. **Decision for the lead (2.6):** use it on Sway, or accept the centered fallback there. The generic fallback stays in place either way.
4. **The 50 ms timer must start when the surface has a frame (`window::Event::Opened`, or better the first presented frame), not when it is requested.** On a cold iced renderer the surface takes 350 to 770 ms to appear. On niri, `enter` follows the first composited frame. Recommendation: arm the 50 ms timer on Opened. Also, if `enter` arrives late (within about 300 ms and before any user input), move the panel to it instead of ignoring it.
5. **The iced_layershell daemon must keep one tiny surface alive for the whole daemon lifetime** (details in iced-layershell.md). Otherwise every popup pays 350 to 770 ms to rebuild the wgpu compositor, which breaks the 100 ms budget, and CLIPBOARD is lost on close. With the keepalive surface, the time from request to pointer position is 12 to 22 ms on KWin.
6. Fractional scale and multiple outputs work. Positions are logical surface-local coordinates, and the per-output surface size is the logical output size. Clamping in logical px gives the right result on KWin and Sway at scale 1.5.

**Go/no-go: GO for D3**, provided that:
- the daemon keeps a keepalive surface;
- the 50 ms fallback is armed on Opened, and late `enter` events move the panel;
- Sway either gets the rebase workaround or documented centered placement;
- the live KWin rows are confirmed (2026-10-23) and match the nested KWin.

Open item for 2.6: the spec says to center on the "most recently used output" when the position is unknown. iced_layershell has no way to report that (see iced-layershell.md). Possible approaches: center on the surface that got keyboard focus, which on KWin and Sway is the **last mapped** surface, so it is arbitrary; use Sway IPC `get_outputs` `focused`; or relax the spec to "the first output".

## Procedures for the PENDING and untested rows

Build once:

```sh
cd /home/axis/Applications/quick-translate
export CARGO_TARGET_DIR=$PWD/target/agent-spikes
cargo build -j 8 --manifest-path spikes/wl-raw-probe/Cargo.toml
cargo build -j 8 --release --manifest-path spikes/overlay-probe/Cargo.toml
```

### Live KWin (done 2026-10-23; how to repeat it)

Before every run, check that the session is unlocked and the screen is on. Don't run while locked or with DPMS off.

```sh
busctl --user call org.freedesktop.ScreenSaver /ScreenSaver org.freedesktop.ScreenSaver GetActive   # must print: b false
kscreen-doctor --dpms show                                                                          # must be on
```

Then leave the mouse still and run:

```sh
timeout 5 target/agent-spikes/debug/wl-raw-probe --ms 1500
timeout 5 target/agent-spikes/debug/wl-raw-probe --ms 1500 --full-buffer
timeout 8 target/agent-spikes/release/overlay-probe --ms 2500 --keepalive
timeout 8 target/agent-spikes/release/overlay-probe --ms 3000 --keepalive --cycles 3
```

What to look for:
- in `wl-raw-probe`, a `pointer enter ... [no motion before it: true]` line before any `first pointer motion` line, and its ms value;
- in `overlay-probe`, `FIRST pointer position via Entered` (not `50 ms fallback fired`) and its `+ms`.

Each run shows a transparent full-screen overlay that grabs the keyboard for 1.5 to 3 s and then exits by itself. Esc also exits `overlay-probe`. With two monitors, put the pointer on the second one and check that the panel appears there.

### Hyprland, real Sway, niri, COSMIC (not tested on the dev machine)

In a real session of that compositor, run the same four commands. On Sway, also run:

```sh
timeout 8 target/agent-spikes/release/overlay-probe --ms 2500 --keepalive --sway-rebase
```

Expected on real Sway: no `enter` without motion, and with `--sway-rebase` a FIRST pointer position line. Record the results in the tables above.

### Nested reproduction (no live session needed)

Nested KWin:

```sh
dbus-run-session -- env XDG_CONFIG_HOME=$(mktemp -d) KWIN_WAYLAND_NO_PERMISSION_CHECKS=1 QT_QPA_PLATFORM=offscreen \
  kwin_wayland --virtual --no-lockscreen --no-global-shortcuts --socket qt-kwin-0 --output-count 1 &
WAYLAND_DISPLAY=qt-kwin-0 target/agent-spikes/debug/fake-pointer 500 400 60 &
WAYLAND_DISPLAY=qt-kwin-0 timeout 5 target/agent-spikes/debug/wl-raw-probe --ms 600
```

`--no-lockscreen` is required. Without it, the nested KWin follows the logind lock and shows its own lock screen, and no surface ever gets focus.

Sway, niri and COSMIC: use an Arch container with `pacman -S sway niri cosmic-comp wayland-utils mesa`. Copy `/usr/bin/sway` once, because the file capabilities on the binary make rootless podman fail with EPERM. Run as a normal user with `XDG_RUNTIME_DIR` set, start sway with `WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman`, symlink `$XDG_RUNTIME_DIR/qt-sway` to `wayland-1`, then run `QT_EXTENT=W,H QT_KEYMAP=/tmp/tr.xkb fake-pointer X Y 900 &`. The keymap comes from `/usr/lib/xkbcommon/xkbcli-compile-keymap --layout tr`.
