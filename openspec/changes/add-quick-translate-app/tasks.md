# Tasks

## 1. Workspace scaffolding

- [x] 1.1 Create the Cargo workspace and the seven crates from design D10 (`quick-translate`, `-core`, `-platform`, `-backends`, `-secrets`, `-ipc`, `-ui`), set `edition = "2024"`, the release profile (`lto = "fat"`, `codegen-units = 1`, `strip = true`) and workspace lints. Verify `cargo build --workspace` and `cargo clippy --workspace -- -D warnings` succeed.
- [x] 1.2 Add `rustfmt.toml`, `.gitignore`, `git init`, and a GitHub Actions workflow running fmt, clippy and tests. Verify the workflow passes locally with `act` or by reading its steps against `cargo` commands that succeed.
- [x] 1.3 Add a `flake.nix` dev shell with the Rust toolchain, `wayland`, `libxkbcommon`, `vulkan-loader`, `pkg-config`, `sway` and `wl-clipboard`. Verify `nix develop -c cargo build` succeeds.

## 2. Week-1 spikes (de-risk before committing to the approach)

- [ ] 2.1 Spike the D3 overlay host: a throwaway iced_layershell binary creates a transparent full-output overlay per output, records the first pointer enter/motion position and draws a panel there. Record per compositor (KWin, Hyprland, Sway, niri; plus COSMIC if available) whether `enter` arrives without motion and the time to first event. Verify the findings are written to `docs/spikes/overlay-probe.md`, with a go/no-go for D3 versus fallback B.
- [x] 2.2 Spike iced_layershell capabilities: dynamically create and destroy layer surfaces per output at runtime, set the input region, use exclusive keyboard interactivity, and get focus immediately on map. Verify with a test binary on KWin and Sway and record the result in `docs/spikes/iced-layershell.md`. If it fails, record the libcosmic evaluation result there instead.
- [ ] 2.3 Spike IME: an iced 0.14 `text_editor` on a layer surface and in a winit window, with fcitx5 and ibus (Turkish/Japanese input). Verify that composed text is committed correctly and record the result in `docs/spikes/ime.md`.
- [ ] 2.4 Spike portal GlobalShortcuts from an unsandboxed binary: install a `.desktop` file, call `Registry.Register`, `CreateSession`, `BindShortcuts(Ctrl+Alt+C)`, and receive `Activated` with an activation token. Run it on KDE Plasma (including re-bind after restart) and GNOME 48+. Measure portal-to-process latency against a native compositor bind. Verify the results are in `docs/spikes/portal-shortcuts.md`.
- [ ] 2.5 Spike the GNOME focus-time PRIMARY read: a winit window created with an activation token reads PRIMARY after keyboard enter. Verify on GNOME that the selected text in another app is returned, both from a portal trigger and from a GNOME custom-shortcut CLI launch, and record the result in `docs/spikes/gnome-primary.md`.
- [ ] 2.6 Review the spike results and update design.md (D3/D4/D5/D6) and the affected specs if any decision changed. Verify `openspec validate add-quick-translate-app --strict` passes.

## 3. Core: config, languages, capabilities

- [x] 3.1 Implement the config model with TOML load and save in `$XDG_CONFIG_HOME/quick-translate/config.toml`: defaults, atomic write (temp file then rename), preservation of unknown keys, and malformed-file fallback to defaults with a notice and the file left untouched. Verify with unit tests for round-trip, unknown keys, a malformed file and a missing directory.
- [x] 3.2 Implement the language list (DeepL target and source codes, display names) and map the system locale (`LC_ALL`/`LC_MESSAGES`/`LANG`) to the default target language, falling back to English. Verify unit tests cover `tr_TR.UTF-8`, `pt_BR`, `C`/`POSIX` and unset variables.
- [x] 3.3 Define the `Capabilities` struct (layer-shell, ext/wlr data-control versions, xdg-activation, fractional scale, viewporter, portal GlobalShortcuts version, compositor identity for snippets only) with `Display` for `--status`. Verify with unit tests on formatting.

## 4. IPC and daemon lifecycle (spec: daemon-lifecycle)

- [x] 4.1 Implement the zbus interface `io.github.quicktranslate.QuickTranslate` (`Toggle`, `Show`, `Settings`, `Quit`, `Status`) with server and client proxy, including the `activation-token` option. Verify with an integration test on a private `dbus-daemon --session` that the client calls reach a mock server.
- [x] 4.2 Implement CLI parsing without initializing the GUI or Tokio on the forward path: commands from the daemon-lifecycle spec, unknown args print usage and exit non-zero, and forward-to-running-daemon happens within 200 ms. Verify with tests for each flag, and measure the forwarding time with `hyperfine` against a running test daemon.
- [x] 4.3 Implement the single-instance race (claim the bus name with do-not-queue, and a losing process exits 0 or forwards) and the Unix-socket fallback when no session bus exists. Verify with a test that starts two daemons concurrently and checks that exactly one survives.
- [x] 4.4 Implement the Wayland-only gate: exit non-zero with a clear message when `WAYLAND_DISPLAY` is unset or the connection fails. Verify with a test that runs the binary with `WAYLAND_DISPLAY` unset.
- [x] 4.5 Implement the D9 desktop integration: compute paths from `$APPIMAGE` or `current_exe()`, write `.desktop`, icon and autostart entries only when their content differs, and add `--remove-integration` (which keeps config and secrets). Verify with unit tests that use a temp `XDG_DATA_HOME`/`XDG_CONFIG_HOME`, including the moved-AppImage case.
- [x] 4.6 Wire the daemon main loop: Tokio runtime, capability probe, integration install before portal registration, warm HTTP client, idle with no timers. Verify the daemon idles at under 0.1% CPU over 60 s with `pidstat`, and that `--status` prints capabilities on the dev machine.
- [x] 4.7 Document the CLI and lifecycle in `README.md` (usage, autostart, removal). Verify the documented commands run as written.

## 5. Platform: Wayland probe and selection capture (spec: selection-capture)

- [x] 5.1 Implement the registry probe on a dedicated connection that fills `Capabilities`. Verify with a headless-sway integration test asserting layer-shell and data-control are detected.
- [x] 5.2 Implement the persistent data-control thread (ext-data-control-v1 preferred, wlr v2 fallback) that tracks the latest PRIMARY offer and its MIME types without reading the data. Verify in headless sway that `wl-copy --primary foo` updates the tracked offer.
- [x] 5.3 Implement the capture pipeline with a `SelectionSource` abstraction: data-control PRIMARY read, then focus-time PRIMARY via the toolkit, then CLIPBOARD, then empty. Apply MIME preference, lossy UTF-8, trimming, the 10,000-character truncation flag and the 150 ms per-step timeout. PRIMARY and CLIPBOARD must never be written during capture. Verify with unit tests using fake sources for every fallback path and the timeout, plus a headless-sway test that reads a real PRIMARY.
- [x] 5.4 Add a test that asserts no input-injection protocol (virtual-keyboard, RemoteDesktop) is ever bound, by checking bound globals in the integration test. Also check that captured text never reaches logs, using a log-capture test.

## 6. Global shortcut (spec: global-shortcut)

- [x] 6.1 Implement the portal flow: `Registry.Register`, `CreateSession`, `BindShortcuts` on every start, `ListShortcuts` for the current trigger, the `Activated` stream with its activation token, and `ConfigureShortcuts` when the version is ≥ 2. A Registry failure falls back to compositor-binding mode. Verify with a mock portal on a private session bus (zbus server implementing the interface) covering bind, activation and v1/v2 behavior.
- [x] 6.2 Implement compositor-binding snippet generation for Sway, Hyprland, niri, river, labwc, Wayfire, COSMIC and generic, using the AppImage path when running from an AppImage. Verify with snapshot tests per compositor.
- [x] 6.3 Implement the opt-in evdev listener: off by default, enabled only after a confirmation warning, with a clear error when input devices are not readable. Verify with unit tests on the key-combo state machine and a test that it never starts while disabled.
- [x] 6.4 Route the portal, CLI and evdev triggers into a single `Daemon::toggle(token)` path that hides the popup if it is visible. Verify with a unit test using a fake UI handle.
- [x] 6.5 Document shortcut setup per environment in `docs/shortcuts.md` (portal desktops and compositor snippets). Verify every snippet matches the generator's snapshot output.

## 7. Translation backends and secrets (spec: translation-backends)

- [x] 7.1 Implement the `Translator` streaming trait, `Request`/`Chunk` types, cancellation, and the shared rustls `reqwest` client with connect timeout 5 s, total timeout 30 s (configurable) and warm-up preconnect. Verify with unit tests for cancellation and timeout using a mock server.
- [x] 7.2 Implement the DeepL backend: `:fx` host choice, a single retry on the other host after 403 with the working host cached, `POST /v2/translate` with omitted `source_lang` for auto, detected-language chunk, `GET /v2/usage` for validation and quota, and error mapping (403, 456, 429 with up to 3 backoff retries, network/timeout). Verify with wiremock tests for every status and the host-switch case.
- [x] 7.3 Implement the OpenAI-compatible backend: `POST {base}/chat/completions` with `stream: true`, an SSE parser, a translation-only system prompt at temperature 0, an optional empty API key, and a "Test connection" call. Verify with wiremock streaming tests (chunked deltas, `[DONE]`, error JSON, mid-stream disconnect) and SSE parser unit tests.
- [x] 7.4 Implement transport rules: refuse plain `http://` for non-loopback hosts unless confirmed, accept loopback `http://`, and send no telemetry. Verify with unit tests on URL validation (127.0.0.1, ::1, localhost, LAN IP, public host).
- [ ] 7.5 Implement Secret Service storage with app/backend attributes and the session-only in-memory fallback when no provider exists (never plaintext). Verify against `gnome-keyring-daemon` in a private D-Bus session in CI, and with a test for the no-provider path.
- [x] 7.6 Document backend setup (DeepL key and quota note, OpenAI-compatible examples for OpenAI, OpenRouter, Ollama) in `docs/backends.md`. Verify the example base URLs pass the transport validation tests.

## 8. UI: popup and settings (specs: translation-popup, translation-backends)

- [x] 8.1 Build the shared iced popup core: two panes with a draggable splitter, editable source with 400 ms debounced re-translation and cancellation, read-only selectable translation, a header with source (Auto plus detected), swap, a remembered target selector and a backend indicator, a loading indicator, a source placeholder while capture is pending, and a truncation notice. Verify with iced update-function unit tests for debounce, cancellation, swap and placeholder replacement.
- [x] 8.2 Implement the copy and dismiss logic: Ctrl+C copies the in-pane selection or else the full translation and closes; Ctrl+C with no translation and no selection does nothing; the Copy button copies and closes; Esc closes without copying; Tab navigation works. Verify with unit tests on the update logic, and manually verify on KDE that the clipboard survives popup close (`wl-paste`).
- [x] 8.3 Implement the layer shell per D3 (or fallback B per 2.6): per-output transparent overlays, first-pointer-wins placement with a 12 px offset, on-output clamping, 50 ms center fallback, outside-click closes, app-drawn edge and corner resize with a minimum size, and size persisted (position not persisted). Verify the clamping math with unit tests and manually on KWin and Sway with a two-monitor or fractional-scale setup.
- [ ] 8.4 Implement the window shell for compositors without layer-shell: a normal window created per activation with the token, centered by the compositor, closing on focus loss, with native resize and size persisted. Verify manually on GNOME, including the focus-time PRIMARY read.
- [ ] 8.5 Follow the system color scheme via the portal Settings `color-scheme`, with live updates. Verify by toggling dark mode on KDE and GNOME while the daemon runs.
- [ ] 8.6 Build the settings app (separate `--settings` process over D-Bus): backend choice; DeepL key field with "Get API key" (opens the browser), validation and quota; OpenAI base URL, model, key and "Test connection"; insecure-URL confirmation; the shortcut section (portal trigger with a Change button, or snippet with a Copy button, plus the evdev opt-in with a warning); autostart toggle; and a Diagnostics section showing capabilities. Verify with update-function tests, and manually that every control persists across restarts.
- [ ] 8.7 Measure the hot path from trigger to the popup's first frame and selection capture, using timestamped debug logs on KDE, Hyprland and GNOME. Verify the popup is visible within 100 ms and record the numbers in `docs/testing.md`.

## 9. Packaging and distribution (spec: distribution)

- [x] 9.1 Write the AppImage build script (`packaging/appimage/build.sh`): build in an `ubuntu:22.04` container, assemble AppRun, `.desktop` and icon, and run appimagetool with the static type2 runtime. Verify the AppImage runs `--version` on the dev machine, with and without `APPIMAGE_EXTRACT_AND_RUN=1`.
- [x] 9.2 Add a bundle audit: fail the build if glibc, libGL, libEGL, libvulkan, libgbm or Mesa libraries are inside the AppImage, and if `ldd` on the binary needs anything outside the allowlist (glibc family, libwayland-client, libxkbcommon). Verify the check fails on a deliberately polluted test AppDir.
- [x] 9.3 Complete the flake: `packages.x86_64-linux.default` and `apps.default` with `wrapProgram` runtime libraries. Verify `nix build` and `nix run . -- --version` succeed.
- [x] 9.4 Add the CI release job: on tag, build the AppImage and `.zsync`, smoke-test them in Ubuntu 22.04, Debian 12 and Fedora containers (`--version`, `--status` gate message with no Wayland), run `nix build`, and attach artifacts to the GitHub Release. Verify on a test tag in a fork or with a dry run.
- [x] 9.5 Document installation (AppImage, NixOS `programs.appimage.binfmt` or `appimage-run`, flake) and the supported-environment matrix in `README.md`. Verify the instructions on Arch (dev machine) and in a NixOS VM or container.

## 10. End-to-end integration checks

- [ ] 10.1 Run the manual matrix from `docs/testing.md` on KDE Plasma 6, GNOME 48+, Hyprland, Sway, niri and COSMIC (where available): trigger, capture, placement, resize, translate with both backends, Ctrl+C copy, Esc dismiss and toggle. Verify every row is recorded as pass or fail with notes, and that failures are filed as follow-up tasks.
- [x] 10.2 Run `openspec validate add-quick-translate-app --strict` and confirm every spec scenario maps to a test or a manual matrix row. Verify the mapping table is in `docs/testing.md`.
