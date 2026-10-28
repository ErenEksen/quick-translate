# Testing

## Hot-path latency

Budget (spec translation-popup, "Fast appearance"): from the trigger reaching the daemon to the
popup on screen in **≤ 100 ms**, with the source text filled in as soon as capture finishes.

### Instrumentation

Run the daemon with debug logs for the quick-translate crates:

```sh
quick-translate --quit
QT_LOG=debug quick-translate --daemon 2>&1 | tee /tmp/qt-hot-path.log
```

`QT_LOG` is an `EnvFilter`. A bare level (`debug`) applies to the quick-translate crates only
and keeps everything else at `warn`; use a full filter such as
`QT_LOG=warn,quick_translate=debug,quick_translate_ui=debug` to choose yourself. Every line has
a UTC timestamp with microseconds. Selected or translated text never appears in the logs, only
lengths.

Per trigger the daemon logs three lines with the same `session` number:

| Line | Logged when | `ms` field |
|---|---|---|
| `hot path: trigger received session=N` | the portal `Activated` signal, the `Toggle`/`Show` D-Bus call or the evdev combo reached the daemon's loop | – |
| `hot path: popup first frame (…) session=N ms=…` | the first frame of the placed panel was drawn (layer shell: first `RedrawRequested` of the winning overlay after placement; window shell: window opened) | since trigger received |
| `hot path: capture done session=N ms=… chars=…` | the captured selection was handed to the popup | since trigger received |

Supporting lines: `showing popup` (the shell got the command), `first overlay mapped`,
`panel placed … by_pointer=true|false` (false means the 50 ms centered fallback), and from the
platform crate `captured selection origin=… step_ms=… total_ms=…`.

Extract the numbers:

```sh
grep -E 'hot path' /tmp/qt-hot-path.log | sed 's/\x1b\[[0-9;]*m//g'
```

Trigger-to-daemon time (key press to `trigger received`) is not in these logs. For the portal
path see `docs/spikes/portal-shortcuts.md` (phase E) for how to compare it with a native
compositor binding.

### Procedure

1. Start the daemon as above, wait 5 s (the first popup after start pays font and glyph cache
   warm-up; record it separately as "first").
2. Select a word in another application, press the shortcut, press Esc. Repeat 10 times with
   the pointer at rest.
3. Repeat with a 2,000-word selection and with an empty selection (CLIPBOARD fallback).
4. Record the median and maximum of `first frame ms` and `capture done ms`.

Use a release build (`cargo build --release`); debug builds are several times slower to draw.

### Results

| Environment | Shortcut path | First popup (ms) | Warm median / max first frame (ms) | Capture done median (ms) | Notes |
|---|---|---|---|---|---|
| KDE Plasma 6 (KWin), layer shell | portal | PENDING | PENDING | PENDING | |
| KDE Plasma 6 (KWin), layer shell | `--toggle` binding | PENDING | PENDING | PENDING | |
| Hyprland, layer shell | `--toggle` binding | PENDING | PENDING | PENDING | |
| GNOME 48+, window shell | portal | PENDING | PENDING | PENDING | focus-time PRIMARY read |
| GNOME 48+, window shell | custom shortcut `--toggle` | PENDING | PENDING | PENDING | no activation token expected |

## Environment knobs for testing

| Variable | Effect |
|---|---|
| `QT_LOG` | Log filter, see "Hot-path latency". |
| `ICED_BACKEND` | Renderer. Default `tiny-skia` (design D4: wgpu measured 233 MB idle RSS and 0.18 % idle CPU on NVIDIA, over the 80 MB budget); `wgpu` restores the GPU renderer. |
| `QT_POPUP_SHELL=window` | Use the window shell even when layer-shell is available (GNOME behaviour on KDE). |
| `QT_SELECTION=focus` | Skip data-control and read the selection through the toolkit after keyboard focus. |
| `QT_HEADLESS=1` | Run the daemon without Wayland or popup surfaces (CLI and IPC tests only). |
| `SWAYSOCK` | When set, the layer shell asks Sway to resend the pointer position once the overlays are mapped (a zero-length `seat - cursor move 0 0` over Sway IPC). |

## Popup shells: manual checks

Live sessions only, never while the screen is locked
(`busctl --user call org.freedesktop.ScreenSaver /ScreenSaver org.freedesktop.ScreenSaver GetActive`
must print `b false`).

```sh
quick-translate --quit
QT_LOG=debug quick-translate --daemon 2>&1 | tee /tmp/qt.log &
quick-translate --status
```

Layer shell (KDE, Sway, Hyprland, niri):

1. Select text, leave the mouse still, press the shortcut: the panel's top-left corner is 12 px
   right of and below the pointer, on the output with the pointer; the log shows
   `panel placed … by_pointer=true`.
2. Repeat with the pointer 20 px from the bottom-right corner: the panel is shifted fully on
   screen.
3. Two monitors, the second at scale 1.5: repeat 1 and 2 on the second monitor.
4. Drag the bottom-right corner to 900x400, press Esc, trigger again: same size, placed at the
   pointer (`[popup] width/height` in `config.toml`). Dragging inward stops at 480x200.
5. Click outside the panel: it closes. Ctrl+C after a translation: `wl-paste` prints it after
   the popup closed.
6. Sway: `panel placed … by_pointer=true` after `nudging the pointer` (requires `SWAYSOCK`); with
   `env -u SWAYSOCK` the panel is centered on the focused output (`centering on the
   compositor's output`) and moves to the pointer if it moves within 300 ms.

Window shell (GNOME; or `QT_POPUP_SHELL=window QT_SELECTION=focus` on KDE):

1. Select text in GNOME Text Editor, press the portal shortcut: a centered window opens with
   focus and the selection (`popup window focused`, `capture done chars=…`).
2. The same through a custom shortcut running `quick-translate --toggle` (no activation token
   expected): record whether focus is granted; without focus the source pane is empty.
3. Click another window: the popup closes. Resize it, close, trigger: the new size is used.

Theme: toggle dark mode in system settings while the daemon runs; the next popup (and an open
one) switches between the light and dark theme.

## Manual matrix

The manual checks for tasks.md 10.1. Run them in live sessions with a release build or the
AppImage, the daemon started as shown in "Popup shells: manual checks", and both backends
configured. Record every cell as `pass`, `fail` (with a note, and file a follow-up task) or
`n/a`. The [scenario mapping](#spec-scenario-mapping) below refers to these row ids.

### Per-environment rows

| Id | Check |
|---|---|
| M1 | **Trigger.** The shortcut shows the popup: the portal shortcut, or a compositor binding that runs `quick-translate --toggle`. On portal desktops, the first start shows the desktop's bind dialog proposing Ctrl+Alt+C. |
| M2 | **Capture.** Selected text appears in the source pane: read in the background on layer-shell compositors, or when the popup gets focus on GNOME. With nothing selected, the CLIPBOARD text is used. With both empty, or only an image on the clipboard, the pane is empty and focused. Afterwards `wl-paste` and `wl-paste --primary` still return their earlier contents. |
| M3 | **Placement.** Layer shell: the top-left corner is 12 px from the pointer, the popup is shifted on screen near the bottom-right edge, and it is placed correctly on a second monitor at scale 1.5. On Sway the popup appears at the pointer without pointer motion. GNOME: a normal window centered by the compositor. |
| M4 | **Resize.** Dragging the bottom-right corner keeps the top-left corner fixed. Shrinking stops at the minimum size. The next popup reuses the size and is placed at the current pointer. |
| M5 | **Translate with DeepL.** The translation is shown, the header shows the detected source language, and the backend indicator shows DeepL. |
| M6 | **Translate with OpenAI-compatible.** The translation streams in progressively, the source shows "Auto" with no detected language, and the indicator shows the backend. Switching the backend in settings takes effect on the next popup. |
| M7 | **Ctrl+C copy.** With no selection, Ctrl+C copies the full translation and closes the popup. With a selection in either pane, it copies only the selection. The Copy button copies and closes. `wl-paste` prints the copied text after the popup closed. Ctrl+C before the translation arrives does nothing. The source application's text is unchanged. |
| M8 | **Esc dismiss.** Esc closes the popup and CLIPBOARD is unchanged. Clicking outside the popup, or the window losing focus, also closes it. |
| M9 | **Toggle.** Triggering again (shortcut and `quick-translate --toggle`) hides the popup. `quick-translate --status` still reports the daemon running with `popup: hidden`. |

| Row | KDE Plasma 6 | GNOME 48+ | Hyprland | Sway | niri | COSMIC |
|---|---|---|---|---|---|---|
| M1 Trigger | PENDING | PENDING | PENDING | PENDING | PENDING | PENDING |
| M2 Capture | PENDING | PENDING | PENDING | PENDING | PENDING | PENDING |
| M3 Placement | PENDING | PENDING (centered) | PENDING | PENDING | PENDING | PENDING |
| M4 Resize | PENDING | PENDING | PENDING | PENDING | PENDING | PENDING |
| M5 DeepL | PENDING | PENDING | PENDING | PENDING | PENDING | PENDING |
| M6 OpenAI-compatible | PENDING | PENDING | PENDING | PENDING | PENDING | PENDING |
| M7 Ctrl+C copy | PENDING | PENDING | PENDING | PENDING | PENDING | PENDING |
| M8 Esc dismiss | PENDING | PENDING | PENDING | PENDING | PENDING | PENDING |
| M9 Toggle | PENDING | PENDING | PENDING | PENDING | PENDING | PENDING |

### One-off checks

Run each of these once, in the environment named.

| Id | Check | Where | Result |
|---|---|---|---|
| X1 | Rebind the shortcut to Super+T in system settings and restart the daemon. Super+T shows the popup without rebinding. | KDE, GNOME | PENDING |
| X2 | **Change shortcut**: with portal v2 it opens the system configuration UI, and with v1 it shows instructions. Rebinding externally while settings is open updates the displayed trigger. | KDE, GNOME | PENDING |
| X3 | Settings shows the active mechanism ("System shortcut (portal)" or the compositor binding). In fallback mode it shows the snippet for the running compositor (and the AppImage path when run from the AppImage), and its Copy button puts the snippet on the clipboard. | KDE, Sway, niri | PENDING |
| X4 | Input-device fallback: enabling it shows the warning and activates only after confirmation. Without access to `/dev/input`, settings reports the missing permission and the daemon keeps running. | any | PENDING |
| X5 | DeepL settings: **Get API key** opens the key page in the browser. A valid key shows as valid with usage `used / limit`. A rejected key shows "Invalid API key". | any | PENDING |
| X6 | OpenAI-compatible settings: **Test connection** against local Ollama without a key succeeds, and with an unknown model it shows the endpoint's error. | any | PENDING |
| X7 | Base URL `http://example.com/v1` is refused until the unencrypted connection is confirmed. `http://127.0.0.1:8080/v1` is accepted. | any | PENDING |
| X8 | The theme follows the portal color-scheme (dark and light), including a switch while the daemon runs. | KDE, GNOME | PENDING |
| X9 | Tab and Shift+Tab reach every popup control. Changing the target language with the arrow keys and Enter starts a new translation. | any | PENDING |
| X10 | After 60 s idle with the popup hidden, `pidstat -p <pid> 60 1` shows under 0.1 % CPU. | KDE | PENDING |
| X11 | The AppImage starts on Ubuntu 22.04 and 24.04 (without `libfuse2t64`), Debian 12, Fedora 40+, openSUSE Tumbleweed and Arch, also with `APPIMAGE_EXTRACT_AND_RUN=1`. | distros | PENDING (`--version` only: see note 1) |
| X12 | On an NVIDIA proprietary driver host, the popup renders with the default renderer and with `ICED_BACKEND=wgpu`. | NVIDIA host | PENDING |
| X13 | `nix run github:<owner>/quick-translate` starts the app in a NixOS Wayland session. | NixOS | PENDING |
| X14 | Started in an X11 session, the AppImage exits non-zero with "requires a Wayland session … (X11 is not supported)". | X11 session | PENDING |
| X15 | A saved key is in the Secret Service (`secret-tool search application io.github.quicktranslate.QuickTranslate`) and not in `config.toml`. With no provider running, settings offers session-only use and the key is gone after a daemon restart. | KDE or GNOME | PENDING |
| X16 | While translating, `ss -tnp` or a packet capture shows connections only to the active backend and the DeepL usage endpoint. | any | PENDING |
| X17 | Hot path: the popup is visible within 100 ms, with a placeholder until capture completes (see [Hot-path latency](#hot-path-latency)). | KDE, Hyprland, GNOME | PENDING |
| X18 | With no backend configured, the translation pane says so and offers a button that opens settings. | any | PENDING |
| X19 | Dragging the splitter resizes the panes. Editing the source re-translates after a 400 ms pause. Swap moves the translation into the source and swaps the languages. | any | PENDING |
| X20 | Turning off **Start on login** removes the autostart entry, and it is not recreated on the next daemon start. | any | PENDING |
| X21 | With an invalid `config.toml`, the daemon starts with defaults, a notice names the file and the parse error, and the file is unchanged until settings are saved. | any | PENDING |
| X22 | Every settings control, and the target language chosen in the popup, survives a daemon restart. `quick-translate --settings` with the daemon running opens the settings window within 200 ms. | any | PENDING |
| X23 | With the compositor-binding fallback active, using settings (including Copy) creates or changes no file under `~/.config/{sway,hypr,niri,river,labwc,wayfire,cosmic}` (`find … -newer <stamp>` lists nothing). | Sway, niri | PENDING |
| X24 | Run the AppImage from `~/Apps/quick-translate.AppImage`: the `.desktop` and autostart `Exec` lines name that path. Move it to `~/bin/qt.AppImage` and start it: both lines are rewritten. | any | pass, see note 2 |
| X25 | README.md lists every environment with its support level and states that X11 is unsupported. | docs review | pass (README.md "Supported environments") |
| X26 | On a portal desktop, after `quick-translate --quit` the shortcut no longer reaches a daemon, and `--status` reports `daemon: not running`. Restarting rebinds it without a dialog. | KDE | PENDING |
| X27 | Run the daemon with `QT_LOG=trace`, then capture and translate a unique word. Neither the log nor any file under `~/.config/quick-translate`, `~/.cache` or `~/.local/share` contains the word (`grep -r`). | any | PENDING |
| X28 | First run with empty XDG directories: DeepL is the active backend, the portal is asked for Ctrl+Alt+C, and the autostart entry is created. | KDE | PENDING (autostart part: see note 2) |
| X29 | Offline (network disabled): the translation pane shows a connection error, and the source pane stays editable. Esc still closes the popup. | any | PENDING |
| X30 | OpenAI-compatible backend with a three-line source text: the translation has three lines and no added commentary. | any | PENDING |

Notes:

1. 2026-10-22: the AppImage built at 20:47 printed its version on the dev machine (CachyOS)
   with and without `APPIMAGE_EXTRACT_AND_RUN=1`. In `ubuntu:22.04` and `ubuntu:24.04`
   containers without libfuse it printed its version with `APPIMAGE_EXTRACT_AND_RUN=1`. In
   those containers, without that variable, it failed to mount when `/dev/fuse` was missing,
   and printed its version when `/dev/fuse` was present even though no `fusermount` was
   installed. In both containers, `--toggle` with no Wayland session printed the Wayland-only
   message and exited 1. In the `nixos/nix` container,
   `nix run nixpkgs#appimage-run -- <AppImage> --version` printed the version. The container
   ran under `--privileged` rootless podman, because bwrap needs user namespaces. Evaluating
   a NixOS system with `programs.appimage = { enable = true; binfmt = true; }` produced the
   binfmt registrations `appimage_type_1` and `appimage_type_2` (nixpkgs 26.11pre). This has
   not been booted in a VM.
2. 2026-10-22, in an isolated session: a private `dbus-run-session` with no service
   activation, temporary `HOME`, `XDG_*` and `XDG_RUNTIME_DIR`, and a nested
   `kwin_wayland --virtual`. Run from `$HOME/Apps/quick-translate.AppImage`, the `Exec` lines
   named that path. After moving it to `$HOME/bin/qt.AppImage` and starting `--daemon`, both
   lines named the new path. `--remove-integration` then removed all three files. On a fresh
   run, the daemon created the autostart entry. The portal part of X28 is still pending
   because no portal was available in that session.

## Spec scenario mapping

`openspec validate add-quick-translate-app --strict` passes (2026-10-22). The tables below map
every scenario in `openspec/changes/add-quick-translate-app/specs/*/spec.md` to the automated
tests that check it (`file::test_fn`) and to manual rows from the [manual matrix](#manual-matrix).

The Coverage column says how much of the scenario the automated tests check:

- **full**: the tests assert every THEN/AND clause, possibly with mocks: wiremock for HTTP, a
  mock portal on a private bus, a headless daemon or fake sources.
- **partial**: some clauses are only covered by the manual row. The Coverage cell names them.
- **manual**: no automated test.

Some automated tests need extra setup and return early, silently, without it:

- `crates/quick-translate-platform/tests/selection_headless.rs` and `wayland_probe.rs` need
  headless Sway (`scripts/headless-sway.sh`, as in CI).
- `crates/quick-translate-secrets/tests/keyring.rs` and
  `crates/quick-translate-ui/tests/settings_persistence.rs` need `dbus-daemon` and
  `gnome-keyring-daemon`.
- Permission tests skip when run as root.

Shell-script checks (`packaging/appimage/*.sh`, `packaging/nix/check-in-container.sh`) do not
run in CI yet (task 9.4).

### daemon-lifecycle

| Scenario | Automated | Manual | Coverage |
|---|---|---|---|
| Started in an X11-only session | `crates/quick-translate/tests/cli.rs::wayland_display_unset_is_refused`, `crates/quick-translate/src/gate.rs::messages_are_clear`, `crates/quick-translate-platform/src/wayland/mod.rs::display_missing_env` | X14 | full (no daemon starts, non-zero exit, message) |
| Wayland socket unreachable | `crates/quick-translate/tests/cli.rs::unreachable_wayland_socket_is_named`, `crates/quick-translate/src/gate.rs::relative_display_without_runtime_dir_fails`, `crates/quick-translate-platform/src/wayland/mod.rs::connect_error_names_socket` | – | full |
| Second invocation forwards its command | `crates/quick-translate/tests/cli.rs::forwarding_is_fast`, `crates/quick-translate/tests/cli.rs::settings_window_is_single_instance_and_closes_with_the_daemon`, `crates/quick-translate/tests/cli.rs::lifecycle_on_the_session_bus` | X22 | partial: headless daemon, and the 200 ms limit is checked on the median of 7 runs |
| Two instances started simultaneously | `crates/quick-translate/tests/cli.rs::two_daemons_started_together_leave_exactly_one`, `crates/quick-translate/tests/cli.rs::two_socket_daemons_started_together_leave_exactly_one`, `crates/quick-translate-ipc/tests/bus.rs::second_claim_loses_with_do_not_queue` | – | full |
| Toggle with no daemon running | `crates/quick-translate/tests/cli.rs::lifecycle_on_the_session_bus`, `crates/quick-translate/tests/cli.rs::lifecycle_on_the_socket_fallback`, `crates/quick-translate/src/runtime/core.rs::capture_result_follows_the_show` | M1, M2, M9 | partial: headless, popup state only |
| Toggle while popup is visible | `crates/quick-translate/tests/cli.rs::lifecycle_on_the_session_bus`, `crates/quick-translate/src/runtime/core.rs::toggle_shows_then_hides_without_waiting` | M9 | full (state level) |
| Quit | `crates/quick-translate/tests/cli.rs::lifecycle_on_the_session_bus`, `crates/quick-translate/tests/cli.rs::daemon_answers_settings_calls`, `crates/quick-translate-platform/tests/shortcut_portal.rs::v2_bind_activate_change_configure_close` | X26 | partial: no test unregisters the portal session from the daemon's quit path. Only `PortalShortcuts::close()` is tested, by itself. |
| Unknown argument | `crates/quick-translate/tests/cli.rs::unknown_argument_prints_usage_without_contacting_the_daemon`, `crates/quick-translate/src/cli.rs::unknown_and_extra_arguments_are_rejected` | – | full |
| Idle daemon uses no CPU | – | X10 | manual |
| Popup hidden, daemon persists | `crates/quick-translate/tests/cli.rs::lifecycle_on_the_session_bus`, `crates/quick-translate/src/runtime/core.rs::popup_closing_itself_resets_visibility` | M8, M9 | partial: real Esc and outside-click dismissal are manual |
| First run without config | `crates/quick-translate-core/src/config.rs::defaults`, `crates/quick-translate-core/src/config.rs::missing_file_and_directory_yield_defaults_and_save_creates_directory` | X28 | partial: unit level, no daemon-level check |
| Malformed config | `crates/quick-translate-core/src/config.rs::malformed_file_yields_defaults_and_is_left_untouched`, `crates/quick-translate-core/src/config.rs::wrong_types_are_malformed`, `crates/quick-translate/src/runtime/persist.rs::malformed_file_is_left_untouched`, `crates/quick-translate-ui/tests/settings_persistence.rs::malformed_config_is_replaced_on_save` | X21 | partial: the notice shown to the user is not asserted |
| Secrets never in config | `crates/quick-translate-ui/tests/settings_persistence.rs::every_control_persists_across_restarts`, `crates/quick-translate-ui/tests/settings_persistence.rs::without_a_keyring_keys_live_in_the_daemon_for_the_session`, `crates/quick-translate-core/src/config.rs::serialized_config_has_no_secret_fields`, `crates/quick-translate-secrets/tests/keyring.rs::gnome_keyring_persists_across_restarts_and_never_in_plaintext` | X15 | full |
| First run from an AppImage | `crates/quick-translate-platform/src/integration.rs::first_install_writes_all_files`, `crates/quick-translate-platform/src/integration.rs::exec_path_prefers_appimage`, `crates/quick-translate-platform/src/integration.rs::paths_follow_xdg_layout` | X24 | full at unit level, end-to-end in X24 |
| AppImage moved | `crates/quick-translate-platform/src/integration.rs::moved_appimage_rewrites_exec_lines` | X24 | full at unit level, end-to-end in X24 |
| Autostart disabled by user | `crates/quick-translate-platform/src/integration.rs::autostart_disabled_removes_entry_and_keeps_it_removed`, `crates/quick-translate/tests/cli.rs::daemon_answers_settings_calls`, `crates/quick-translate-ui/src/settings/tests.rs::simple_controls_save_immediately` | X20 | full (no real daemon restart) |
| Remove integration | `crates/quick-translate/tests/cli.rs::remove_integration_stops_the_daemon_and_keeps_config`, `crates/quick-translate-platform/src/integration.rs::remove_deletes_integration_but_keeps_config` | – | full |

### distribution

| Scenario | Automated | Manual | Coverage |
|---|---|---|---|
| No libfuse2 on Ubuntu 24.04 | – | X11 | manual |
| Extract-and-run mode | `packaging/appimage/build.sh` (smoke test: `APPIMAGE_EXTRACT_AND_RUN=1 … --version`) | X11 | partial: `--version` only |
| Oldest supported glibc | `packaging/appimage/audit.sh` (glibc 2.35 symbol baseline, `ldd` check), `packaging/appimage/build.sh` (`--version` in ubuntu:22.04) | X11 | partial: no Wayland session |
| Rolling release | `packaging/appimage/build.sh` (host smoke test `--version`) | X11 | partial: `--version` only |
| Bundle content check | `packaging/appimage/audit.sh`, `packaging/appimage/test-audit.sh` | – | full |
| Proprietary GPU driver host | – | X12 | manual |
| nix run | `packaging/nix/check-in-container.sh` (`nix run . -- --version`) | X13 | partial: local flake, no Wayland session |
| nix build | `packaging/nix/check-in-container.sh` (`nix build`, `test -x result/bin/quick-translate`) | – | full |
| Documentation lists the matrix | – | X25 | manual (docs review) |
| Unsupported session refused | `crates/quick-translate/tests/cli.rs::wayland_display_unset_is_refused`, `crates/quick-translate/src/gate.rs::messages_are_clear` | X14 | partial: runs the cargo binary with `WAYLAND_DISPLAY` unset, not the AppImage in an X11 session |

### global-shortcut

| Scenario | Automated | Manual | Coverage |
|---|---|---|---|
| Shortcut shows popup | `crates/quick-translate/src/runtime/core.rs::toggle_shows_then_hides_without_waiting`, `crates/quick-translate/src/runtime/core.rs::every_trigger_source_shares_the_path`, `crates/quick-translate-platform/tests/shortcut_portal.rs::v2_bind_activate_change_configure_close` | M1 | partial: mock portal and fake UI |
| Shortcut hides popup | `crates/quick-translate/src/runtime/core.rs::toggle_shows_then_hides_without_waiting`, `crates/quick-translate/src/runtime/core.rs::hide_when_hidden_sends_nothing`, `crates/quick-translate-ui/src/window/tests.rs::toggling_show_and_hide_repeatedly` | M9 | full (toggle path), real key press manual |
| Portal available | `crates/quick-translate-platform/tests/shortcut_portal.rs::v2_bind_activate_change_configure_close`, `crates/quick-translate-ui/src/settings/tests.rs::views_build_in_every_state` | M1, X3 | partial: mock portal, and the settings label is not rendered |
| Portal missing | `crates/quick-translate-platform/tests/shortcut_portal.rs::missing_portal_falls_back_to_compositor`, `crates/quick-translate-platform/tests/shortcut_portal.rs::no_capability_skips_the_portal`, `crates/quick-translate-platform/tests/shortcut_portal.rs::interface_missing_is_unavailable`, `crates/quick-translate/src/settings.rs::compositor_info_carries_the_snippet` | X3 | partial: no real Sway |
| First bind on KDE | `crates/quick-translate-platform/tests/shortcut_portal.rs::v2_bind_activate_change_configure_close` | M1 | partial: Register first, then BindShortcuts with `CTRL+ALT+c`. The KDE dialog is manual. |
| Binding survives restart | `crates/quick-translate-platform/tests/shortcut_portal.rs::binds_on_every_start_and_keeps_persisted_trigger` | X1 | partial: mock portal |
| Registry failure | `crates/quick-translate-platform/tests/shortcut_portal.rs::registry_failure_falls_back_to_compositor` | – | partial: fallback asserted. The log line, and that the daemon keeps running, are not. See [Gaps](#gaps). |
| Change via portal v2 | `crates/quick-translate-ui/src/settings/tests.rs::portal_v2_change_opens_the_portal_and_v1_shows_instructions`, `crates/quick-translate-ipc/tests/bus.rs::settings_methods_and_signals_reach_mock_server`, `crates/quick-translate-platform/tests/shortcut_portal.rs::v2_bind_activate_change_configure_close` | X2 | partial: each link tested separately |
| Change on an older portal | `crates/quick-translate-platform/tests/shortcut_portal.rs::v1_offers_no_configure`, `crates/quick-translate-ui/src/settings/tests.rs::portal_v2_change_opens_the_portal_and_v1_shows_instructions` | X2 | full (logic level) |
| Shortcut changed externally | `crates/quick-translate-platform/tests/shortcut_portal.rs::v2_bind_activate_change_configure_close`, `crates/quick-translate-ipc/tests/bus.rs::settings_methods_and_signals_reach_mock_server`, `crates/quick-translate-ui/src/settings/tests.rs::trigger_updates_live` | X2 | partial: each link tested separately |
| Snippet on niri | `crates/quick-translate-platform/tests/shortcut_snippet.rs::snippets_per_compositor`, `crates/quick-translate-platform/tests/shortcut_snippet.rs::compositor_maps_to_format`, `crates/quick-translate-core/src/capabilities.rs::detect_compositor`, `crates/quick-translate-ui/src/settings/tests.rs::compositor_snippet_copy` | X3 | partial: copy is checked as an effect, not on the real clipboard |
| Snippet references the AppImage path | `crates/quick-translate-platform/tests/shortcut_snippet.rs::appimage_path_is_used_from_env`, `crates/quick-translate-platform/src/shortcut/snippet.rs::appimage_wins` | X3 | full |
| Config files untouched | – | X23 | manual |
| Enabling shows warning | `crates/quick-translate-ui/src/settings/tests.rs::evdev_needs_confirmation_and_reports_permission_errors`, `crates/quick-translate-platform/src/shortcut/evdev.rs::default_mode_is_disabled`, `crates/quick-translate-platform/tests/shortcut_evdev.rs::never_starts_while_disabled` | X4 | full (update level) |
| No device permission | `crates/quick-translate-platform/tests/shortcut_evdev.rs::unreadable_device_reports_permission`, `crates/quick-translate-platform/tests/shortcut_evdev.rs::unreadable_directory_reports_permission`, `crates/quick-translate-ui/src/settings/tests.rs::evdev_needs_confirmation_and_reports_permission_errors` | X4 | partial: no test enables the fallback in a real daemon |
| Activation not blocked by pending translation | `crates/quick-translate/src/runtime/core.rs::toggle_shows_then_hides_without_waiting`, `crates/quick-translate-ui/src/popup/tests.rs::reset_cancels_and_keeps_ids_increasing`, `crates/quick-translate-ui/src/popup/tests.rs::closing_cancels_in_flight_request`, `crates/quick-translate-ui/src/host.rs::close_cancels_and_reports_once` | X17 | partial: no single test with a translation in flight when the toggle arrives |

### selection-capture

| Scenario | Automated | Manual | Coverage |
|---|---|---|---|
| ext-data-control available | `crates/quick-translate-platform/tests/selection_headless.rs::reads_real_primary`, `crates/quick-translate-platform/src/wayland/mod.rs::data_control_preference`, `crates/quick-translate-ui/src/popup/tests.rs::capture_replaces_placeholder_and_starts_translation` | M2 | full when headless Sway advertises ext-data-control |
| Only wlr-data-control available | `crates/quick-translate-platform/src/wayland/mod.rs::data_control_preference`, `crates/quick-translate-core/src/capabilities.rs::data_control_requires_wlr_v2`, `crates/quick-translate-platform/tests/selection_headless.rs::reads_real_primary` | M2 | full |
| wlr-data-control too old for PRIMARY | `crates/quick-translate-core/src/capabilities.rs::data_control_requires_wlr_v2`, `crates/quick-translate-platform/src/wayland/data_control.rs::wlr_v1_is_unsupported`, `crates/quick-translate/src/runtime/core.rs::without_data_control_the_shell_captures` | – | full |
| GNOME without data-control | `crates/quick-translate/src/runtime/core.rs::every_trigger_source_shares_the_path`, `crates/quick-translate/src/runtime/core.rs::without_data_control_the_shell_captures`, `crates/quick-translate-ui/src/window/tests.rs::focus_capture_waits_for_focus_and_display`, `crates/quick-translate-platform/src/selection/tests.rs::empty_primary_falls_back_to_focus_read` | M2 (GNOME) | partial: the token reaches the show request, but its use for xdg-activation is not asserted |
| Focus not granted | `crates/quick-translate-ui/src/window/tests.rs::focus_wait_over_starts_capture_without_focus`, `crates/quick-translate-ui/src/clipboard.rs::closed_gate_fails_at_the_deadline`, `crates/quick-translate-ui/src/clipboard.rs::no_focus_capture_is_fast_and_empty` | M2 (GNOME) | partial: fakes only |
| Nothing selected, clipboard has text | `crates/quick-translate-platform/src/selection/tests.rs::failing_steps_fall_back_to_clipboard`, `crates/quick-translate-platform/tests/selection_headless.rs::falls_back_to_clipboard_then_empty`, `crates/quick-translate-ui/src/clipboard.rs::capture_falls_through_to_clipboard` | M2 | full |
| Nothing selected, clipboard empty | `crates/quick-translate-platform/src/selection/tests.rs::nothing_anywhere_is_empty`, `crates/quick-translate-platform/tests/selection_headless.rs::falls_back_to_clipboard_then_empty`, `crates/quick-translate-ui/src/popup/tests.rs::empty_capture_focuses_empty_source_without_request` | M2 | full |
| Clipboard holds only an image | `crates/quick-translate-platform/src/selection/tests.rs::image_only_clipboard_is_empty`, `crates/quick-translate-platform/tests/selection_headless.rs::falls_back_to_clipboard_then_empty` | M2 | full |
| Rich text source | `crates/quick-translate-platform/src/selection/tests.rs::mime_preference`, `crates/quick-translate-platform/tests/selection_headless.rs::prefers_plain_utf8_over_rich_text` | – | full |
| Surrounding whitespace | `crates/quick-translate-platform/src/selection/tests.rs::normalize_trims_but_keeps_line_breaks`, `crates/quick-translate-platform/tests/selection_headless.rs::reads_real_primary` | – | full |
| Oversized selection | `crates/quick-translate-platform/src/selection/tests.rs::normalize_truncates_by_characters`, `crates/quick-translate-platform/src/selection/tests.rs::oversized_selection_sets_truncated`, `crates/quick-translate-platform/tests/selection_headless.rs::truncates_oversized_primary`, `crates/quick-translate-ui/src/popup/tests.rs::truncation_is_reported` | – | full (the notice is asserted as state, not as rendered text) |
| Whitespace-only selection | `crates/quick-translate-platform/src/selection/tests.rs::whitespace_only_primary_is_empty`, `crates/quick-translate-platform/tests/selection_headless.rs::falls_back_to_clipboard_then_empty` | – | full |
| Source application does not answer | `crates/quick-translate-platform/src/selection/tests.rs::unanswered_step_times_out_and_falls_through`, `crates/quick-translate-platform/src/selection/tests.rs::custom_step_timeout`, `crates/quick-translate-platform/src/wayland/data_control.rs::read_to_end_times_out_when_owner_is_silent` | – | full |
| Clipboard preserved | `crates/quick-translate-platform/tests/selection_headless.rs::capture_preserves_primary_and_clipboard` | M2 | full for data-control. The GNOME focus-time path is manual only. |
| No synthetic copy | `crates/quick-translate-platform/tests/wayland_probe.rs::data_control_binds_no_input_injection_protocol`, `crates/quick-translate-platform/tests/wayland_probe.rs::no_input_injection_api_in_sources` | – | full |
| Logs after capture | `crates/quick-translate-platform/src/selection/tests.rs::captured_text_never_reaches_logs`, `crates/quick-translate-platform/tests/selection_headless.rs::captured_text_never_reaches_logs`, `crates/quick-translate-platform/src/selection/tests.rs::debug_output_hides_text` | X27 | partial: only capture-phase logs are tested. Translation-phase logs and files on disk are not. |

### translation-backends

| Scenario | Automated | Manual | Coverage |
|---|---|---|---|
| Default backend | `crates/quick-translate-core/src/config.rs::defaults`, `crates/quick-translate-ui/src/settings/tests.rs::input_is_ignored_until_loaded`, `crates/quick-translate-ui/tests/settings_persistence.rs::every_control_persists_across_restarts` | – | full |
| Switch backend | `crates/quick-translate-ui/src/settings/tests.rs::simple_controls_save_immediately`, `crates/quick-translate/src/runtime/backend.rs::openai_key_is_optional`, `crates/quick-translate/tests/cli.rs::daemon_answers_settings_calls` | M6 | partial: no test checks that the daemon sends the next request to the new backend after a reload |
| Backend not configured | `crates/quick-translate-ui/src/popup/tests.rs::unconfigured_backend_sends_nothing_and_opens_settings`, `crates/quick-translate-ui/src/popup/tests.rs::failures_are_shown_and_not_configured_offers_settings`, `crates/quick-translate/src/runtime/backend.rs::deepl_without_key_is_not_configured`, `crates/quick-translate-backends/tests/deepl.rs::empty_key_is_not_configured`, `crates/quick-translate-backends/tests/openai.rs::missing_model_is_not_configured` | X18 | partial: the rendered message is not asserted |
| Key saved | `crates/quick-translate-secrets/tests/keyring.rs::gnome_keyring_round_trip_with_attributes`, `crates/quick-translate-secrets/tests/keyring.rs::gnome_keyring_persists_across_restarts_and_never_in_plaintext`, `crates/quick-translate-core/src/config.rs::serialized_config_has_no_secret_fields` | X15 | full (private gnome-keyring) |
| No keyring available | `crates/quick-translate-ui/src/settings/tests.rs::without_a_keyring_saving_offers_session_only_use`, `crates/quick-translate-ui/tests/settings_persistence.rs::without_a_keyring_keys_live_in_the_daemon_for_the_session`, `crates/quick-translate-secrets/tests/keyring.rs::no_provider_falls_back_to_session_only`, `crates/quick-translate-secrets/tests/keyring.rs::unreachable_bus_falls_back_to_session_only` | X15 | full |
| Get API key | `crates/quick-translate-ui/src/settings/tests.rs::get_api_key_opens_the_deepl_key_page` | X5 | partial: checks the open-URL effect only |
| Valid key | `crates/quick-translate-ui/src/settings/tests.rs::valid_deepl_key_is_validated_then_stored_with_its_host`, `crates/quick-translate-backends/tests/deepl.rs::usage_reports_quota` | X5 | full (wiremock) |
| Invalid key | `crates/quick-translate-ui/src/settings/tests.rs::invalid_deepl_key_is_not_stored`, `crates/quick-translate-backends/tests/deepl.rs::usage_with_invalid_key`, `crates/quick-translate-backends/tests/deepl.rs::forbidden_on_both_hosts_is_invalid_key` | X5 | full (the "Invalid API key" text is not asserted) |
| Free key | `crates/quick-translate-backends/src/deepl.rs::host_for_key`, `crates/quick-translate-backends/tests/deepl.rs::fx_key_uses_free_host_and_other_keys_use_pro` | – | full |
| Endpoint fallback | `crates/quick-translate-backends/tests/deepl.rs::forbidden_on_pro_switches_to_free_once_and_caches_it`, `crates/quick-translate-backends/tests/deepl.rs::cached_host_is_used_first`, `crates/quick-translate-backends/src/deepl.rs::cached_host_wins_over_suffix` | – | full |
| Auto-detect | `crates/quick-translate-backends/tests/deepl.rs::auto_detect_omits_source_lang_and_reports_detected`, `crates/quick-translate-backends/src/deepl.rs::body_omits_source_lang_for_auto`, `crates/quick-translate-ui/src/popup/tests.rs::deltas_accumulate_and_detected_language_is_shown` | M5 | full |
| Quota display | `crates/quick-translate-ui/src/settings/tests.rs::opening_with_a_stored_deepl_key_shows_the_quota`, `crates/quick-translate-backends/tests/deepl.rs::usage_reports_quota` | X5 | full |
| Quota exceeded | `crates/quick-translate-backends/tests/deepl.rs::quota_exceeded_456`, `crates/quick-translate-ui/src/popup/tests.rs::failures_are_shown_and_not_configured_offers_settings` | M5 | full (the pane text is not asserted) |
| Rate limited | `crates/quick-translate-backends/tests/deepl.rs::rate_limited_twice_then_succeeds`, `crates/quick-translate-backends/tests/deepl.rs::rate_limited_gives_up_after_three_retries_with_backoff` | – | full |
| Offline | `crates/quick-translate-backends/tests/deepl.rs::network_failure_is_connection_error`, `crates/quick-translate-backends/tests/http.rs::connect_timeout_is_bounded`, `crates/quick-translate-ui/src/popup/tests.rs::translation_stream_stops_at_first_error` | X29 | partial: "popup remains usable" is manual |
| Local server without key | `crates/quick-translate-backends/tests/openai.rs::request_body_and_no_auth_header_without_key`, `crates/quick-translate-backends/tests/openai.rs::test_connection_success`, `crates/quick-translate-ui/src/settings/tests.rs::test_connection_uses_the_fields_and_reports_the_result` | X6 | full (wiremock) |
| Hosted service with key | `crates/quick-translate-backends/tests/openai.rs::bearer_token_when_key_is_set` | – | full |
| Wrong model | `crates/quick-translate-backends/tests/openai.rs::error_json_for_unknown_model`, `crates/quick-translate-backends/tests/openai.rs::status_mapping`, `crates/quick-translate-ui/src/settings/tests.rs::test_connection_uses_the_fields_and_reports_the_result` | X6 | full |
| Progressive output | `crates/quick-translate-backends/tests/openai.rs::deltas_arrive_progressively`, `crates/quick-translate-backends/tests/openai.rs::streams_chunked_deltas_until_done`, `crates/quick-translate-ui/src/popup/tests.rs::deltas_accumulate_and_detected_language_is_shown` | M6 | full (rendering is manual) |
| Multi-line text | `crates/quick-translate-backends/tests/openai.rs::request_body_and_no_auth_header_without_key`, `crates/quick-translate-backends/src/openai.rs::prompt_mentions_target_and_only_translation` | X30 | partial: only the prompt is tested. Real model output is manual. |
| Popup closed during streaming | `crates/quick-translate-backends/tests/openai.rs::cancel_mid_stream_aborts_the_request`, `crates/quick-translate-backends/tests/openai.rs::dropping_the_stream_aborts_the_request`, `crates/quick-translate-ui/src/popup/tests.rs::closing_cancels_in_flight_request`, `crates/quick-translate-ui/src/host.rs::close_cancels_and_reports_once` | – | full (as a chain of tests) |
| Slow endpoint | `crates/quick-translate-backends/tests/openai.rs::stalled_stream_times_out`, `crates/quick-translate-backends/tests/deepl.rs::timeout_is_reported`, `crates/quick-translate-backends/tests/http.rs::backend_timeout_overrides_client_timeout`, `crates/quick-translate-backends/tests/http.rs::client_total_timeout_applies` | – | full (wiring the configured timeout into the daemon's backend is untested) |
| Insecure remote endpoint | `crates/quick-translate-ui/src/settings/tests.rs::insecure_remote_url_is_refused_until_confirmed`, `crates/quick-translate-ui/tests/settings_persistence.rs::insecure_endpoint_persists_only_after_confirmation`, `crates/quick-translate-backends/src/transport.rs::remote_http_needs_confirmation`, `crates/quick-translate-backends/tests/openai.rs::insecure_remote_endpoint_is_refused_without_a_request` | X7 | full |
| Loopback over HTTP | `crates/quick-translate-backends/src/transport.rs::loopback_http_is_accepted`, `crates/quick-translate-ui/src/settings/tests.rs::https_and_loopback_urls_clear_the_insecure_flag`, `crates/quick-translate-backends/tests/docs.rs::example_base_urls_pass_without_confirmation` | X7 | full |
| No telemetry | `crates/quick-translate-backends/tests/http.rs::redirects_are_not_followed` | X16 | partial: only redirects are tested. Which hosts get contacted is manual. |

### translation-popup

| Scenario | Automated | Manual | Coverage |
|---|---|---|---|
| Popup content | `crates/quick-translate-ui/src/popup/tests.rs::capture_replaces_placeholder_and_starts_translation`, `crates/quick-translate-ui/src/popup/tests.rs::deltas_accumulate_and_detected_language_is_shown`, `crates/quick-translate-ui/src/popup/tests.rs::view_builds_in_every_state` | M2, M5 | partial: the rendered header is not asserted |
| Splitter drag | `crates/quick-translate-ui/src/popup/split.rs::ratio_tracks_cursor_inside_bounds`, `crates/quick-translate-ui/src/popup/tests.rs::splitter_is_clamped_and_persisted_on_release`, `crates/quick-translate-ui/src/popup/tests.rs::splitter_moves_with_arrow_keys` | X19 | partial: ratio math only |
| User edits source | `crates/quick-translate-ui/src/popup/tests.rs::edit_cancels_in_flight_request`, `crates/quick-translate-ui/src/popup/tests.rs::stale_chunks_are_ignored`, `crates/quick-translate-ui/src/popup/tests.rs::continuous_typing_sends_one_request_after_the_pause` | X19 | full (update level) |
| Continuous typing | `crates/quick-translate-ui/src/popup/tests.rs::continuous_typing_sends_one_request_after_the_pause` | X19 | full (update level) |
| Default from locale | `crates/quick-translate-core/src/language.rs::turkish_locale`, `crates/quick-translate-core/src/language.rs::posix_precedence`, `crates/quick-translate-core/src/language.rs::unset_empty_and_unsupported_fall_back_to_english`, `crates/quick-translate-core/src/config.rs::effective_target_lang` | – | full |
| Remembered choice | `crates/quick-translate-ui/src/popup/tests.rs::target_selection_persists_and_retranslates`, `crates/quick-translate/src/runtime/core.rs::persisted_choices_apply_to_the_next_popup`, `crates/quick-translate/src/runtime/persist.rs::changes_merge_into_the_file_and_keep_unknown_keys` | X22 | full (as a chain, no daemon restart) |
| Swap | `crates/quick-translate-ui/src/popup/tests.rs::swap_with_detected_source`, `crates/quick-translate-ui/src/popup/tests.rs::swap_with_explicit_source_and_regional_target` | X19 | full (update level) |
| Cursor in middle of screen | `crates/quick-translate-ui/src/layer/geometry.rs::pointer_in_the_middle_puts_the_corner_next_to_it`, `crates/quick-translate-ui/src/layer/placement.rs::first_pointer_wins_and_losers_go` | M3 | partial: math and state machine only |
| Cursor near bottom-right edge | `crates/quick-translate-ui/src/layer/geometry.rs::pointer_near_bottom_right_shifts_up_and_left`, `crates/quick-translate-ui/src/layer/placement.rs::output_resize_keeps_panel_on_screen` | M3 | partial: math only |
| Second monitor with fractional scale | `crates/quick-translate-ui/src/layer/geometry.rs::fractional_second_output_uses_its_logical_size` | M3 | partial: math only |
| Sway without pointer motion | `crates/quick-translate-platform/src/sway.rs::sends_one_run_command`, `crates/quick-translate-ui/src/layer/placement.rs::late_enter_moves_the_panel_to_the_pointer_output`, `crates/quick-translate-ui/src/layer/placement.rs::late_enter_is_ignored_after_the_window_or_input` | M3 (Sway) | partial: no real Sway |
| Cursor position unknown | `crates/quick-translate-ui/src/layer/geometry.rs::centered_fallback`, `crates/quick-translate-ui/src/layer/placement.rs::fallback_centers_on_a_compositor_chosen_overlay`, `crates/quick-translate-ui/src/layer/placement.rs::single_output_uses_the_compositor_chosen_overlay_directly` | M3 | partial: the 50 ms timer is not tested |
| GNOME | `crates/quick-translate-ui/src/window/tests.rs::opened_window_reports_shown`, `crates/quick-translate-ui/src/window/tests.rs::focus_loss_after_focus_closes`, `crates/quick-translate-ui/src/window/tests.rs::escape_closes_at_once`, `crates/quick-translate/src/runtime/core.rs::without_data_control_the_shell_captures` | M1–M9 (GNOME) | partial: shell logic only |
| Resize on layer-shell compositor | `crates/quick-translate-ui/src/layer/geometry.rs::bottom_right_drag_keeps_top_left_fixed`, `crates/quick-translate-ui/src/layer/geometry.rs::grips` | M4 | partial: math only |
| Minimum size | `crates/quick-translate-ui/src/layer/geometry.rs::shrinking_stops_at_minimum`, `crates/quick-translate-ui/src/layer/geometry.rs::persisted_size_below_minimum_is_raised` | M4 | partial: math only |
| Size remembered | `crates/quick-translate-ui/src/window/tests.rs::resize_then_close_persists_size_once`, `crates/quick-translate/src/runtime/core.rs::persisted_choices_apply_to_the_next_popup` | M4 | partial: the layer shell does not test saving the size |
| Warm trigger | `crates/quick-translate/src/runtime/core.rs::toggle_shows_then_hides_without_waiting`, `crates/quick-translate-ui/src/popup/tests.rs::opens_with_placeholder_and_no_request`, `crates/quick-translate-ui/src/popup/tests.rs::capture_replaces_placeholder_and_starts_translation` | X17 | partial: the 100 ms time to first frame is not measured (Results above: PENDING) |
| Slow selection owner | `crates/quick-translate/src/runtime/core.rs::toggle_shows_then_hides_without_waiting`, `crates/quick-translate-ui/src/popup/tests.rs::edits_are_ignored_while_capture_pending`, `crates/quick-translate-ui/src/host.rs::capture_of_another_session_is_ignored` | X17 | partial: a fake capture that never finishes, not a real owner taking 140 ms |
| Ctrl+C without selection | `crates/quick-translate-ui/src/popup/tests.rs::ctrl_c_without_selection_copies_full_translation_and_closes`, `crates/quick-translate-ui/src/popup/tests.rs::key_classification` | M7 | partial: the real CLIPBOARD write is manual |
| Ctrl+C with selection | `crates/quick-translate-ui/src/popup/tests.rs::ctrl_c_copies_translation_selection`, `crates/quick-translate-ui/src/popup/tests.rs::ctrl_c_copies_source_selection` | M7 | partial: as above |
| Paste after close | `crates/quick-translate-ui/src/popup/tests.rs::copy_button_copies_full_translation_and_closes` | M7 | partial: that the clipboard stays owned after close is manual |
| Ctrl+C while translation pending | `crates/quick-translate-ui/src/popup/tests.rs::ctrl_c_while_pending_does_nothing` | M7 | full (update level) |
| Esc | `crates/quick-translate-ui/src/popup/tests.rs::escape_closes_without_copying`, `crates/quick-translate-ui/src/window/tests.rs::escape_closes_at_once` | M8 | full (update level). CLIPBOARD unchanged is manual. |
| Click outside | `crates/quick-translate-ui/src/window/tests.rs::focus_loss_after_focus_closes` | M8 | partial: window shell only. The layer shell's outside press is untested. |
| Toggle | `crates/quick-translate/src/runtime/core.rs::toggle_shows_then_hides_without_waiting`, `crates/quick-translate/tests/cli.rs::lifecycle_on_the_session_bus`, `crates/quick-translate-ui/src/window/tests.rs::toggling_show_and_hide_repeatedly` | M9 | full (daemon level, headless) |
| After copy | `crates/quick-translate-platform/tests/wayland_probe.rs::data_control_binds_no_input_injection_protocol`, `crates/quick-translate-platform/tests/wayland_probe.rs::no_input_injection_api_in_sources` | M7 | partial: no input-injection API is used, but the source application's text is checked manually |
| Dark mode | `crates/quick-translate-ui/src/theme.rs::light_unless_dark_is_preferred`, `crates/quick-translate-platform/src/appearance.rs::only_prefer_dark_is_dark` | X8 | partial: mapping only |
| Runtime change | `crates/quick-translate/src/runtime/core.rs::theme_changes_reach_the_shell_once`, `crates/quick-translate-platform/src/appearance.rs::signal_values` | X8 | partial: no live portal signal |
| Change target language by keyboard | `crates/quick-translate-ui/src/popup/tests.rs::target_selector_by_keyboard`, `crates/quick-translate-ui/src/popup/tests.rs::target_chosen_with_arrows_starts_after_debounce`, `crates/quick-translate-ui/src/popup/tests.rs::tab_cycles_through_all_enabled_controls` | X9 | full (update level) |

### Summary

113 scenarios in 6 spec files:

| Spec | Scenarios |
|---|---|
| daemon-lifecycle | 17 |
| distribution | 10 |
| global-shortcut | 16 |
| selection-capture | 16 |
| translation-backends | 25 |
| translation-popup | 29 |

- **108** have at least one automated check:
  - 102 are covered by cargo tests.
  - 6 (distribution) are covered only by the packaging shell scripts.
- **57** are fully covered by automated checks and **51** partially. The clauses the
  automated checks leave out are covered by the manual row named in the table.
- **5** have no automated test and are covered by a manual row only: Idle daemon uses no CPU
  (X10), No libfuse2 on Ubuntu 24.04 (X11), Proprietary GPU driver host (X12), Documentation
  lists the matrix (X25) and Config files untouched (X23).
- **91** have a manual row.
- Every scenario has an automated test, a manual row, or both.

### Gaps

Every scenario has an automated test or a manual row. The following are still weak, and are
the first candidates for new tests:

- **Registry failure** (global-shortcut): no manual row, because a Registry failure cannot
  easily be provoked on a real desktop. The test asserts the fallback but not the logged
  error, and does not check that the daemon keeps running.
- **Quit** (daemon-lifecycle): nothing tests the daemon's quit path unregistering the portal
  session (`runtime/service.rs` has no tests). X26 checks it only indirectly.
- **GNOME without data-control** (selection-capture): no test asserts that the activation
  token is passed to xdg-activation. Spike 2.5 (`docs/spikes/gnome-primary.md`) is still
  open.
- **Logs after capture** (selection-capture): only capture-phase logs are tested.
  Translation-phase logs and files on disk rely on X27.
- **Switch backend** (translation-backends): the daemon's rebuild of the backend after a
  config reload is untested. Only M6 checks that the next request goes to the new backend.
- **Activation not blocked by pending translation** (global-shortcut): no single test
  toggles while a translation request is in flight.
- **No telemetry** (translation-backends): only "redirects are not followed" is automated.
  The set of contacted hosts relies on X16.

General limits:

- No popup text is checked as rendered. Popup and settings views are only built in every
  state (`view_builds_in_every_state`, `views_build_in_every_state`), so header labels and
  error messages are checked only as state, not as text.
- `crates/quick-translate-ui/src/layer/mod.rs`, `layer/overlay.rs`, `shell.rs`,
  `crates/quick-translate/src/runtime/mod.rs` and `runtime/service.rs` have no unit tests. This
  leaves untested the layer shell's outside-press close and size persistence, the Sway pointer
  nudge wiring, the color-scheme watch and the config-reload handling.
- Some tests skip silently without headless Sway, `dbus-daemon` or `gnome-keyring-daemon`.
  CI provides headless Sway and installs `gnome-keyring`.
- `crates/quick-translate-backends/tests/http.rs::live_tls_to_deepl_rejects_bogus_key` is
  `#[ignore]`d because it needs the network.
- The packaging scripts do not run in CI (task 9.4).
- The manual matrix has not been run yet (task 10.1). Only X24 and X25 are recorded, plus
  parts of X11 and X28. Until it is run, every manual-only clause is unverified.

## Release dry run

Task 9.4: `.github/workflows/release.yml` runs on a `v*` tag (and manually through
`workflow_dispatch`, which is a dry run that skips the `release` job). Jobs:

| Job | Runs in | What it does |
|---|---|---|
| `version` | ubuntu-24.04 | Version = tag without `v`; fails unless it equals `[workspace.package] version` |
| `appimage` | `ubuntu:22.04` job container | `build.sh --no-container --install-deps --update-info "gh-releases-zsync\|<owner>\|<repo>\|latest\|…"`, uploads the AppImage and `.zsync` |
| `smoke` | `ubuntu:22.04`, `debian:12`, `fedora:44` job containers (no FUSE) | `smoke-test.sh --no-audit --require-zsync <AppImage> <version>` on the bare image, then binutils and `audit.sh` |
| `nix` | ubuntu-24.04 + `cachix/install-nix-action@v31` | `nix build`, `nix run . -- --version` must print `quick-translate <version>` |
| `release` | ubuntu-24.04, tag pushes only, `contents: write` | `SHA256SUMS`, then `softprops/action-gh-release@v2` with the AppImage, `.zsync` and `SHA256SUMS` (prerelease when the version has a `-`) |

`packaging/appimage/smoke-test.sh [--audit|--no-audit] [--require-zsync] <AppImage> [expected-version]`
is shared by CI and local runs. Every launch uses `env -i` with a throwaway `HOME`,
`XDG_RUNTIME_DIR` and `TMPDIR`, so it never reaches the desktop session. It checks `--version`
(extract-and-run, and FUSE when `/dev/fuse` exists), `--help`, an unknown argument (exit 2), the
Wayland gate ("Unsupported session refused": with `WAYLAND_DISPLAY` unset and `DISPLAY=:0`,
`XDG_SESSION_TYPE=x11`, the commands `quick-translate`, `--daemon`, `--toggle` and `--status`
exit 1 with `quick-translate requires a Wayland session: WAYLAND_DISPLAY is not set (X11 is not
supported)`; `--daemon` with a missing socket exits 1 with `… cannot connect to the Wayland
compositor at …`), that the `.zsync` describes this file (Filename, Length, SHA-1) and matches the
embedded update-information pattern, and optionally the bundle audit. The binary needs only
`libc`, `libm` and `libgcc_s` at load time (Wayland libraries are dlopened), so minimal images
work; the script itself needs only bash, coreutils and grep.

### Results (2026-10-22, local podman, no `act`)

AppImage rebuilt from the current tree with `packaging/appimage/build.sh` (ubuntu:22.04
container, appimagetool 1.9.1, static type2 runtime 20251108):
`quick-translate-0.1.0-x86_64.AppImage`, 10,000,888 bytes,
sha256 `af316edce7affe25a8ccba994e4ab4f8517985682d9375724d2d60488c6dad0a`; `.zsync` 34,433 bytes.
Build-time audit passed (DT_NEEDED `libgcc_s.so.1`, `libm.so.6`, `libc.so.6`; newest symbol
`GLIBC_2.35`).

| Environment | Bare image (`--no-audit --require-zsync`) | With binutils (`--audit`) | Notes |
|---|---|---|---|
| `ubuntu:22.04` (glibc 2.35) | 11 passed, 0 failed, 2 skipped (FUSE, audit); exit 0 | 12 passed, 1 skipped (FUSE); audit passed; exit 0 | |
| `debian:12` (glibc 2.36) | 11 passed, 0 failed, 2 skipped; exit 0 | 12 passed, 1 skipped; audit passed; exit 0 | also passed with a non-executable copy (as after an artifact download) |
| `fedora:44` (glibc 2.43; `fedora:latest` = 44, 45 is still a prerelease image) | 11 passed, 0 failed, 2 skipped; exit 0 | 12 passed, 1 skipped; audit passed; exit 0 | |
| Host: CachyOS (Arch), glibc 2.44 | – | 14 passed, 0 skipped (FUSE checks included); exit 0 | `./…AppImage --version` and `APPIMAGE_EXTRACT_AND_RUN=1 ./…AppImage --version` both print `quick-translate 0.1.0`, exit 0 |

In every environment: `--version` printed `quick-translate 0.1.0` (exit 0), each no-Wayland
command exited 1 with the gate message above and empty stdout, and the `.zsync` matched
(SHA-1 `fffe58d8f92b10b6e48863d4396eeb3057b765a6`). Negative check: expected version `9.9.9`
fails with `FAIL: extract-and-run: --version prints 0.1.0, expected 9.9.9`, exit 1.

Nix: `packaging/nix/check-in-container.sh run` (docker.io/nixos/nix) built
`quick-translate-0.1.0.drv` from the current tree, `result/bin/quick-translate` is executable, and
`nix run . -- --version` printed `quick-translate 0.1.0`, exit 0.

Workflow lint: `podman run --rm -v "$PWD:/repo:ro" -w /repo docker.io/rhysd/actionlint:latest`
(actionlint 1.7.12, with shellcheck on the `run:` scripts) reports 0 errors for `release.yml`
and `ci.yml`; `shellcheck packaging/appimage/smoke-test.sh` is clean.

Not verified: a real tag push on GitHub (job containers, artifact upload and download, the
Release upload and `GITHUB_TOKEN` permissions). Before the first release, push a test tag such as
`v0.1.0` in a fork (or run the workflow manually for a dry run) and check that the Release holds
the AppImage, `.zsync` and `SHA256SUMS`, and that the embedded update information names the
repository that built it.
