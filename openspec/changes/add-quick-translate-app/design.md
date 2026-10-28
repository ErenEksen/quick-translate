# Design

## Context

Greenfield: the repository contains only `openspec/`. Motivation and scope are in proposal.md, and behavior contracts are in `specs/*/spec.md`. This document covers how to build it.

The approach is shaped by constraints of Wayland's security model, all confirmed by research in October 2026:

- There is no standard way for a client to grab global keys, learn the global cursor position, or place a toplevel at absolute coordinates.
- The GlobalShortcuts portal exists on KDE (xdg-desktop-portal-kde), GNOME 48+ (bind fixed in 49) and Hyprland. It is missing from the wlr, COSMIC, GTK and XApp backends and from niri. Interface v2 (xdg-desktop-portal 1.21, Jan 2026) adds `ConfigureShortcuts` and activation tokens on `Activated`. Since xdg-desktop-portal-kde 6.7.4, bindings must be re-bound on every session.
- ext-data-control-v1 is available on KWin 6.7, Hyprland 0.52, Sway 1.11, niri 26.04, COSMIC and labwc. wlr-data-control v2 (which includes PRIMARY) is available on Sway, Hyprland, niri, labwc, Wayfire and COSMIC. KWin dropped the wlr variant in 6.5. Mutter has neither, and upstream declined to add them.
- wlr-layer-shell is supported everywhere except Mutter, including Muffin, the Cinnamon compositor. Layer surfaces cannot be moved or resized by the compositor.
- DeepL has no OAuth, only `Authorization: DeepL-Auth-Key`. The free API tier changed in 2026 to a one-time 1M-character Developer credit.
- Unsandboxed apps must call `org.freedesktop.host.portal.Registry.Register` with an app ID that matches an installed `.desktop` file before using portals.

## Goals / Non-Goals

**Goals:**
- Hot path: from the trigger reaching the daemon to the popup on screen with source text visible in **≤ 100 ms**. The first translation byte depends on the network, and the HTTP connection to the backend is opened in parallel with selection capture to hide TLS setup.
- One codebase whose behavior adapts at runtime to the protocols present, with no compile-time per-desktop builds.
- Idle daemon: about 0% CPU, with a resident memory budget of **≤ 80 MB** to be measured in the spike. Exceeding it triggers the renderer decision described in D4.
- A self-contained AppImage that depends only on host glibc ≥ 2.35, libwayland-client, libxkbcommon and GPU drivers.

**Non-Goals:**
- X11 or XWayland support of any kind.
- Typing into or modifying the source application, and synthetic input of any kind.
- A GNOME Shell extension. GNOME gets a centered window.
- Translation history, OCR, offline models, and backends beyond DeepL and OpenAI-compatible.
- Flatpak or Snap packaging. Sandboxing would block data-control on niri and complicate the portal app ID.
- A Windows or macOS port.

## Decisions

### D1. Resident daemon with D-Bus single instance (vs. spawn per hotkey)
One process stays alive for the whole session. It owns:
- the Wayland connection(s),
- the GUI runtime with fonts and the renderer already initialized,
- a warm HTTP client and connection pool,
- the portal session,
- the clipboard source after a copy. Wayland clipboard data is served by the owning client, so a short-lived process would empty the clipboard when it exited.

The daemon claims the session-bus name `io.github.quicktranslate.QuickTranslate` and exports `/io/github/quicktranslate/QuickTranslate` with methods `Toggle(a{sv})`, `Show(a{sv})`, `Settings()`, `Quit()` and `Status() -> a{sv}`. The options dict carries `activation-token`, taken from the CLI's `XDG_ACTIVATION_TOKEN` environment variable when a compositor or launcher provides one.

The settings process (D4) also needs daemon-owned state, so the interface adds `ReloadConfig()`, `SetSessionSecret(s backend, s key)`, `ShortcutInfo() -> a{sv}`, `ConfigureShortcut(s activation_token)`, `SetEvdevEnabled(b)` and the signals `ShortcutChanged(s trigger)` and `SettingsRequested()`.

The CLI path parses arguments with a minimal parser and does not initialize the GUI or Tokio unless it ends up becoming the daemon, so forwarding stays under 200 ms. zbus's blocking API starts a Tokio runtime internally, so the forward path uses a small blocking D-Bus client over the raw socket (about 3 ms per forward).

*Alternatives considered:*
- **Spawn per hotkey:** about 200–400 ms cold start (iced/egui benchmark), font scanning on every launch, and loss of the clipboard on exit. Rejected.
- **Unix socket only:** simpler, but D-Bus gives activation semantics and introspection for free. A socket at `$XDG_RUNTIME_DIR/quick-translate.sock` is kept only as a fallback if no session bus exists, which is rare.

### D2. Runtime capability probe drives every platform choice
At daemon start, one registry roundtrip on a dedicated Wayland connection records the presence and versions of:
- `zwlr_layer_shell_v1`
- `ext_data_control_manager_v1`
- `zwlr_data_control_manager_v1` (v≥2)
- `xdg_activation_v1`
- `wp_fractional_scale_manager_v1`
- `wp_viewporter`

The daemon also checks whether the portal exposes `org.freedesktop.portal.GlobalShortcuts`, and at which version. The compositor identity (`XDG_CURRENT_DESKTOP`, plus `HYPRLAND_INSTANCE_SIGNATURE`, `SWAYSOCK`, `NIRI_SOCKET`, etc.) is used **only** to choose which binding snippet to show in settings, never for behavior. The result is a `Capabilities` struct passed to each subsystem. It is shown in settings ("Diagnostics") and printed by `quick-translate --status`.

*Alternative considered:* branching on the desktop name. This is brittle because capabilities change across compositor versions. Rejected.

### D3. Popup shell: full-output transparent overlay that hosts the popup (layer-shell compositors)
On trigger, the daemon creates one layer surface **per output** with these properties:
- layer `overlay`, anchored on all four edges, exclusive zone -1,
- keyboard interactivity `exclusive`,
- fully transparent.

The first surface to receive a pointer `enter` or `motion` event wins. The others are destroyed immediately. The winner then draws the popup panel at the cursor's surface-local position (offset by 12 px), clamped inside the output. These are logical coordinates, and fractional scale is applied by the toolkit.

The full-output host replaces four separate mechanisms with one:
1. **Cursor position:** comes from pointer enter. No compositor IPC is needed.
2. **Output selection:** the output that has the pointer wins.
3. **Moving and resizing:** the panel is a widget inside our own surface, so drag-to-resize and the splitter are pure redraws with no `set_size`/`set_margin` roundtrips, which makes them smooth.
4. **Outside-click dismissal:** a click on the transparent area outside the panel closes the popup. Exclusive keyboard interactivity guarantees focus, so `Ctrl+C` and `Esc` work immediately.

If no pointer event arrives within 50 ms of the overlays first mapping (not of the request), the popup is placed centered on an overlay created without an output, so the compositor picks its active output, and the remaining overlays are destroyed. A late `enter` may still move the panel to the cursor, because niri sends it only after its first frame. The overlay surfaces are destroyed on hide.

Sway sends `enter` only after pointer motion (spike 2.1). When `SWAYSOCK` is set and no pointer event has arrived, the daemon sends one `seat - cursor move 0 0` command over the Sway IPC socket. This is a zero-length move that makes Sway emit `enter` with the real position within about 1 ms. It never sends key or text input and is not used for selection capture.

A permanent 1×1 transparent keep-alive layer surface lives for the daemon's lifetime (spike 2.2). Without it, closing the last surface tears down the renderer, so every popup costs 345–770 ms, and the CLIPBOARD source dies about 70 ms after the popup closes.

*Alternatives considered:*
- **(B)** A separate 1×1-buffer probe (with viewporter destination), then a popup-sized layer surface placed with margins. This uses less GPU memory while visible, but it needs app-drawn resize through `set_size`/`set_margin` roundtrips and has no outside-click detection. Rejected after spike 2.1: the probe surface gets `enter` exactly when the overlay would, so B fixes nothing.
- **Compositor IPC for the cursor position** (`hyprctl cursorpos`, KWin scripting): per-compositor and unnecessary given A. The only IPC use is the Sway nudge above, which still reads the position from `enter`.

### D4. GUI stack: iced 0.14 with iced_layershell, sharing one app core across three shells
The popup and settings UI are written once as an iced `update`/`view` core in `quick-translate-ui`. Three thin shells host that core:

| Shell | Used when | Surface |
|---|---|---|
| `layer` (iced_layershell) | `zwlr_layer_shell_v1` present | overlay surfaces from D3, created and destroyed at runtime through its layer messages (`NewLayerShell`, `RemoveWindow`, `SetInputRegion`) |
| `window` (iced/winit) | no layer-shell (GNOME) | normal xdg_toplevel, created per activation with the activation token and destroyed on hide. Mutter centers it, and resizing is compositor-native. |
| `settings` (iced/winit) | `--settings` | a **separate process** in normal-window mode that talks to the daemon over D-Bus. It is not latency-critical, which keeps the layer shell free of xdg_toplevel needs. |

Renderer: tiny-skia by default, with `ICED_BACKEND=wgpu` as an override. Measured on the dev machine (NVIDIA), wgpu idled at 233 MB RSS and 0.18% CPU, over both the 80 MB budget and the idle-CPU requirement. tiny-skia idled at 24 MB and 0.00% with similar first-frame times. In the window shell, wgpu would also pay 300–540 ms per popup, because iced drops its renderer with the last window.

Spike 2.2 confirmed iced_layershell can create and destroy per-output surfaces at runtime, set the input region and take exclusive keyboard focus on map. It cannot enumerate outputs, so the daemon lists outputs on its own Wayland connection and targets surfaces by output name. Its Focused/Unfocused events are unreliable, so the layer shell closes on outside click and Esc, not on focus loss.

IME: iced 0.14 supports input methods through text-input-v3. Spike 2.3 verified the protocol plumbing and layout-based Turkish input on both shells; composition with fcitx5 and ibus is still checked in the manual matrix.

*Alternatives considered:*
- **libcosmic:** supports layer surfaces and windows in one runtime and is the fallback if iced_layershell cannot create and destroy per-output surfaces dynamically or lacks input-region control. It was not chosen as the default because it is a git-only dependency, ships COSMIC theming, and its API moves fast.
- **GTK4 + gtk4-layer-shell:** the best IME support, but GTK4 is painful to bundle in an AppImage (gdk-pixbuf loaders, schemas, typelibs) and has the slowest startup.
- **Raw SCTK + cosmic-text:** maximum control, but text editing and IME would have to be hand-written.
- **Slint, egui:** no usable layer-shell support.

### D5. Selection capture runs on its own thread with a persistent data-control device
When data-control is available, a dedicated thread holds a persistent Wayland connection with an ext-data-control device (or wlr v2). It tracks the **latest PRIMARY offer and its MIME types** without reading any data. On trigger, the offer is read with a 150 ms timeout in preferred MIME order: `text/plain;charset=utf-8`, `UTF8_STRING`, `text/plain`. This avoids the connect and roundtrip cost of a one-shot read.

Without data-control (GNOME), the `window` shell maps with the activation token and reads PRIMARY through the toolkit's primary-clipboard API after the keyboard `enter` event. Per the spec, the selection is sent before keyboard focus. smithay-clipboard's `load_primary()` blocks with no timeout, so the read runs on a helper thread with the 150 ms limit and a short retry loop, because the offer can arrive just after focus (spike 2.5). CLIPBOARD and an empty pane follow, as specified in `selection-capture`.

No input simulation is used anywhere. A deliberate omission is the XWayland PRIMARY read on GNOME, which would contradict the non-goal.

### D6. Global shortcut: portal first, generated snippet otherwise, with one trigger path
The portal flow uses ashpd with the `global_shortcuts` feature:
1. `Registry.Register(app_id)`
2. `CreateSession`
3. `BindShortcuts([{id:"translate-selection", preferred_trigger:"CTRL+ALT+c"}])`, run on **every** daemon start to work around KDE 6.7.4
4. `ListShortcuts` to display the actual trigger
5. Subscribe to `Activated`, whose `activation_token` (portal v2) flows into D3/D4/D5

"Change shortcut" calls `ConfigureShortcuts` when the version is ≥ 2.

Spike 2.4 findings on KDE:
- `Register` fails unless the `.desktop` file exists, which confirms the D9 ordering.
- A failed `Register` does not make `CreateSession` fail. The portal falls back to the caller's systemd scope (for example `org.kde.konsole`), so a `Register` error ends the portal path and the daemon falls back to compositor bindings.
- xdg-desktop-portal binds an app ID to a connection on its first call, including property reads. The version is therefore probed on a separate throwaway connection.
- KDE 6.8 sends no activation token on `Activated`, which is harmless because KDE uses the layer shell. The `timestamp` is always 0, so it is never used for latency.
- Re-binding on every start is safe: a user's rebind persists through `BindShortcuts` with the original preferred trigger, and no dialog appears again. `ShortcutsChanged` reaches the running daemon.
- The portal adds 1–2 ms over kglobalaccel. It is about 27 ms faster than a native command shortcut, which has to start a process. The "recommend native" rule therefore does not apply on KDE.

Without the portal, settings renders a snippet for the detected compositor that runs `quick-translate --toggle`, and the CLI forwards to `Toggle`. Both paths converge on the same `Daemon::toggle(token)` call.

The evdev listener is a separate opt-in module. It is off by default and is not compiled out, because it carries no dependency cost.

### D7. Backends behind one streaming trait
```
trait Translator {
    async fn translate(&self, req: Request, cancel: CancellationToken)
        -> Result<impl Stream<Item = Result<Chunk>>>;
}
```
`Request` holds the text, the source language (optional, meaning auto) and the target language. `Chunk` is either `Delta(String)` or `Detected(lang)`.

- **DeepL:** a single `POST /v2/translate` that emits one `Delta` plus `Detected(detected_source_language)`.
  - The host is chosen by the `:fx` suffix. A 403 triggers one retry on the other host, and the working host is cached in config.
  - `GET /v2/usage` backs the quota display and key validation.
  - Error mapping: 403 invalid key, 456 quota exceeded, 429 retried with exponential backoff up to 3 times, plus timeouts.
- **OpenAI-compatible:** `POST {base}/chat/completions` with `stream: true`.
  - Responses are parsed as SSE `data:` lines and mapped to `Delta` chunks.
  - The system prompt asks for the translation only, preserving line breaks and formatting. Temperature is 0.
  - There is no `Detected` chunk.

Both use one shared `reqwest` client with rustls (no OpenSSL dependency, which keeps the AppImage portable), HTTP/2 and keep-alive. On trigger, the daemon starts a connection warm-up to the active backend host in parallel with selection capture.

Cancellation happens on popup hide, on source edit (debounced 400 ms), and when the backend is switched.

### D8. Config and secrets
- **Config:** TOML at `$XDG_CONFIG_HOME/quick-translate/config.toml`. It holds the backend choice, target language, OpenAI base URL and model, popup size, splitter ratio, cached DeepL host, and the autostart and evdev flags.
  - Writes are atomic: write a temp file, then rename.
  - Unknown keys are preserved.
- **Secrets:** stored through Secret Service (the `oo7` crate, a pure-Rust D-Bus client) with attributes `{application: io.github.quicktranslate.QuickTranslate, backend: deepl|openai}`. If there is no provider, secrets are held in memory for the session only, as the spec requires, and are never written in plaintext.

### D9. Desktop integration installed by the binary itself
On every daemon start, the binary computes the expected files from `$APPIMAGE`, falling back to `current_exe()`:
- `~/.local/share/applications/io.github.quicktranslate.QuickTranslate.desktop`, with `Exec=<path> --settings`
- `~/.local/share/icons/hicolor/scalable/apps/io.github.quicktranslate.QuickTranslate.svg`
- `~/.config/autostart/io.github.quicktranslate.QuickTranslate.desktop`, with `Exec=<path> --daemon`, when autostart is enabled

Files are rewritten only when their content differs, which covers the AppImage being moved or updated. `--remove-integration` deletes them. This must happen before `Registry.Register`, because the portal resolves the app ID through the `.desktop` file.

### D10. Workspace layout
```
Cargo.toml                       (workspace)
crates/
  quick-translate/               bin: CLI dispatch, daemon wiring
  quick-translate-core/          config, languages/locale, Capabilities, errors
  quick-translate-platform/      wayland probe, data-control thread, portal shortcuts,
                                 host registry, desktop integration, evdev (opt-in)
  quick-translate-backends/      Translator trait, DeepL, OpenAI-compatible
  quick-translate-secrets/       Secret Service wrapper + session-only fallback
  quick-translate-ipc/           zbus interface (server + client proxy)
  quick-translate-ui/            iced core (popup + settings views), layer/window shells
packaging/
  appimage/  (AppRun, .desktop, icon, build script)
  nix/       (flake.nix at repo root references this)
```
A single async runtime (Tokio) is shared: iced's tokio executor, zbus (tokio feature) and ashpd (tokio).

### D11. Build and distribution
- **AppImage:** built in an `ubuntu:22.04` container (glibc 2.35 baseline) with `cargo build --release --locked`, `lto = "fat"`, `codegen-units = 1` and `strip = true`, then assembled with appimagetool using the **static type2 runtime**, so the host needs no libfuse2.
  - Bundled: the binary, `.desktop` file, icon and AppRun.
  - Not bundled: glibc, libwayland-client, libxkbcommon, libGL/libEGL/libvulkan/libgbm. These come from the host and are dlopened or linked.
  - The CI smoke test checks `--version`, runs with `APPIMAGE_EXTRACT_AND_RUN=1`, and runs `ldd` against an allowlist.
- **Nix flake:** `packages.x86_64-linux.default` and `apps.default` via `rustPlatform.buildRustPackage`, using `wrapProgram` to put `wayland`, `libxkbcommon`, `vulkan-loader` and `libGL` on `LD_LIBRARY_PATH`. The flake also has a `devShells.default`.
- **CI:** GitHub Actions with fmt, clippy (`-D warnings`), tests, AppImage build and smoke test, and `nix build`.

### D12. Testing strategy
- **Unit tests:** config round-trip, locale-to-language mapping, `:fx` host choice, error mapping, SSE parser, selection fallback ordering via a fake `SelectionSource`, and panel clamping math.
- **Backend tests:** a mock HTTP server (wiremock) for DeepL and OpenAI responses, including streaming, 403/456/429 and timeouts.
- **Headless Wayland integration tests:** in CI, `sway` with `WLR_BACKENDS=headless` covers layer-shell and wlr/ext data-control: set PRIMARY with `wl-copy --primary`, then assert the capture returns it. A `kwin_wayland --virtual` job is optional if the runner supports it.
- **Manual matrix:** a checklist in `docs/testing.md` covering KDE, GNOME, Hyprland, Sway, niri and COSMIC, with hot-path latency measured with timestamped logs (`QT_LOG=debug`).

## Risks / Trade-offs

- **Pointer `enter` not sent until the pointer moves on some compositors.** Mitigation: 50 ms timeout, then center the popup, as decided in D3. Week-1 spike across KWin, Hyprland, Sway and niri.
- **Full-output overlay surfaces cost GPU memory while visible** (about 33 MB per 4K output), and a compositor may animate their appearance. Mitigation: they exist only while the popup is shown, and fallback B is described in D3.
- **iced_layershell API gaps** in dynamic per-output surfaces or input regions. Mitigation: spike first, with libcosmic as the documented alternative (D4).
- **The GNOME focus-time PRIMARY read breaks** if Mutter tightens focus-stealing rules or the activation token is missing (for example, a custom shortcut launching the CLI without a token). Mitigation: fall back to CLIPBOARD or an empty pane, and document the limitation.
- **KDE portal quirks** (re-bind each session, bind dialog on first run). Mitigation: bind on every start and show the actual trigger in settings.
- **Portal activation latency is unmeasured.** Mitigation: measure it in the spike, and if it exceeds about 50 ms, recommend the native compositor binding in settings even where the portal exists.
- **DeepL free tier is now a one-time credit.** Mitigation: the quota display and a clear 456 message pointing to the AI backend.
- **IME in the iced text editor** has not been verified with fcitx5 or ibus. Mitigation: week-1 check, with GTK4 or libcosmic as an escalation path, at the cost of the toolkit decision.
- **Privacy:** the selected text leaves the machine. Mitigation: it goes only to the configured backend, HTTPS is required for non-loopback hosts unless the user explicitly overrides it, there is no telemetry, and selected text is never written to disk or logs.
- **Unsandboxed AppImage portal identification** is inferred, not verified. Mitigation: D9 installs the `.desktop` file before `Register`, and the spike verifies this on KDE and GNOME.

## Migration Plan

Greenfield, so there is nothing to migrate. Release flow: tag, CI builds the AppImage and Nix outputs, and the GitHub Release attaches the AppImage plus its `.zsync`. Rollback means using the previous release's AppImage. Config is forward-compatible because unknown keys are preserved.

## Open Questions

- Whether to also build aarch64 AppImages. This depends on CI runner availability and does not change the design.
- The exact default popup size and splitter ratio, to be tuned during UI work (initially 720×320 logical px, 50/50).
- Whether `--status` output should be machine-readable JSON as well. This is cosmetic.
