# Spike 2.4: GlobalShortcuts portal from an unsandboxed binary

Status: **KDE Plasma 6.8 verified** (2026-10-23): probe, first bind with the dialog, activation, re-bind after restart, `ConfigureShortcuts`, a user rebind surviving restart, latency against a native bind, and cleanup. **GNOME 48+ is not tested**, because the dev machine has no GNOME.

Code: `spikes/portal-shortcuts/` (standalone crate, own `[workspace]`).
Binary: `target/agent-portal-spike/debug/portal-shortcuts-spike`.
Interactive walkthrough: `spikes/portal-shortcuts/run-interactive.sh`.

## Environment (dev machine, 2026-10-22)

| Component | Version |
|---|---|
| Session | KDE Plasma 6.8 Wayland (`XDG_CURRENT_DESKTOP=KDE`) |
| kwin / plasma-workspace | 6.8.0 |
| xdg-desktop-portal (frontend) | 1.22.1 |
| xdg-desktop-portal-kde | 6.8.0 (impl `GlobalShortcuts.version` = 2) |
| kglobalaccel | runs inside `kwin_wayland` (bus name `org.kde.kglobalaccel`) |
| ashpd / zbus / tokio | 0.13.13 / 5.19.0 / 1.54.1 |
| rustc | 1.99.1 |

Spike app ID: `io.github.quicktranslate.QuickTranslate.Spike`. Its `.desktop` file is `~/.local/share/applications/io.github.quicktranslate.QuickTranslate.Spike.desktop`, with `NoDisplay=true`. It never collides with the real app ID.

## Method

`portal-shortcuts-spike` has these phases:

- `--probe` (non-interactive) does the following:
  1. Writes the `.desktop` file.
  2. Calls `ashpd::register_host_app_with_connection` (`org.freedesktop.host.portal.Registry.Register`).
  3. Reads the Registry `version`.
  4. Calls `Register` a second time, which must fail. That failure proves the first call stuck to this connection.
  5. Builds the `GlobalShortcuts` proxy, which reads the `version` property.
  6. Calls `CreateSession`, then `ListShortcuts`.
  7. Dumps the spike's kglobalaccel component, if one exists.
  8. Calls `Session.Close`.

  It **never calls `BindShortcuts`**. Every call has a 10 s timeout, so an unexpected dialog would show up as a timeout, not a hang. The `--no-register` and `--no-desktop` flags reproduce the failure modes.
- `--bind [--listen S] [--configure] [--exec CMD]` does the following:
  1. Calls Register, CreateSession and `ListShortcuts`. This first `ListShortcuts` shows the persisted binds.
  2. Subscribes to Activated, Deactivated and ShortcutsChanged, and to every `org.kde.kglobalaccel.Component.globalShortcutPressed` on the session bus.
  3. Calls `BindShortcuts([translate-selection, preferred_trigger=CTRL+ALT+c])`, then `ListShortcuts` again.
  4. If `--configure` is given, calls `ConfigureShortcuts`.
  5. Listens. For each `Activated` it prints:
     - the options keys and whether `activation_token` is present
     - the portal `timestamp` compared against CLOCK_MONOTONIC, BOOTTIME and REALTIME, because the spec says "millisecond granularity, with an undefined base"
     - the hop from the kglobalaccel signal to the portal signal
     - the key-down-to-arrival time, when `--evdev-log` data is available

  `--exec` re-launches a command with `XDG_ACTIVATION_TOKEN` set from the activation.
- `--listen-only` runs the same listener but makes no portal calls.
- `--stamp` is the client for a native KDE custom shortcut, the analogue of `quick-translate --toggle`. It records CLOCK_REALTIME and CLOCK_MONOTONIC as the first statement of `main()`, then sends them to the listener over `$XDG_RUNTIME_DIR/qt-portal-spike.sock`.
- `--evdev-log --sock PATH /dev/input/eventN…` is an optional root helper. It provides ground truth for key-press time. It forwards only the kernel timestamps of non-modifier key-downs pressed while Ctrl, Alt or Meta is held, which keeps it from logging ordinary typing.
- `--cleanup` removes the `.desktop` file, the socket and the stamp log. It also unregisters the spike's kglobalaccel actions and calls `Component.cleanUp()`.

I checked from source that the probe cannot open a dialog:

- The `GlobalShortcuts.xml` docs mention a dialog only for `BindShortcuts` ("will typically result the portal presenting a dialog").
- In xdg-desktop-portal-kde `globalshortcuts.cpp`:
  - `CreateSession` only creates the session and calls `loadActions()`.
  - `ListShortcuts` only returns the session's current descriptions.
  - `BindShortcuts` opens a `QuickDialog` **only for shortcut IDs that were never persisted**. Known IDs are applied without a dialog.

## Results so far (verified on KDE Plasma 6.8)

Probe output (`--probe`, run from a Konsole tab):

```
[registry] Register(io.github.quicktranslate.QuickTranslate.Spike) OK in 7.9ms
[registry] org.freedesktop.host.portal.Registry version = 1
[registry] second Register rejected as documented: org.freedesktop.portal.Error.Failed: Could not register app ID: Connection already associated with an application ID
[gs] org.freedesktop.portal.GlobalShortcuts version = 2 (3.0ms)
[gs] CreateSession OK in 15.0ms: Session("/org/freedesktop/portal/desktop/session/1_22310/ashpd_RsYwkRj7Cq")
[gs] ListShortcuts OK in 8.6ms
[list] 0 shortcuts
[kglobalaccel] component /component/io_github_quicktranslate_QuickTranslate_Spike not present
[gs] Session.Close OK
```

Findings:

1. **Registry.Register works for an unsandboxed binary**, once the matching `.desktop` file exists. It took about 8 ms. A second Register on the same connection is rejected, as documented.
2. **`.desktop` is required.** With `--no-desktop`, Register fails with `App info not found for 'io.github.quicktranslate.QuickTranslate.Spike'`. The frontend creates registered host apps with `XDP_APP_INFO_FLAG_REQUIRE_GAPPINFO` and resolves `<app_id>.desktop` through GDesktopAppInfo. This confirms D9's ordering: install the `.desktop` file, then Register.
3. **Pitfall: a failed or skipped Register does not mean CreateSession fails.** xdg-desktop-portal also infers a host app ID from the caller's systemd user unit (`app-<id>-….scope`).
   - From a Konsole tab (cgroup `app-org.kde.konsole-3606.scope/tab(…).scope`), `--no-register` and `--no-desktop` still got a working session. By the frontend source (`get_app_from_pid`), that session is attributed to `org.kde.konsole`.
   - From a neutral scope (`systemd-run --user --scope … --no-register`), CreateSession fails with `NotAllowed: An app id is required`.

   Consequence for the daemon: **a Register error must abort the portal path** and switch to the fallback, as the spec says. The daemon must never continue to CreateSession, or it may bind shortcuts under the terminal's or launcher's app ID.
4. **GlobalShortcuts `version` = 2** on xdg-desktop-portal 1.22.1 with xdp-kde 6.8. The frontend advertises `MIN(impl_version, 2)`. So `ConfigureShortcuts` is available on KDE, and the "Change via portal v2" scenario applies. ashpd exposes this as `GlobalShortcuts::version()`, read once when the proxy is built.
5. **CreateSession and ListShortcuts show no dialog.** They took about 15 ms and 8 ms, and the probe has no side effects: `~/.config/kglobalshortcutsrc` is byte-identical before and after, and no kglobalaccel component is created. A fresh app ID's `ListShortcuts` before any bind returns 0 shortcuts. After a bind it returns the persisted binds (phase B below).
6. Cleanup was verified. No spike `.desktop` file, socket or kglobalaccel component is left behind.

### ashpd 0.13.13 API notes

- Registry support is `ashpd::register_host_app(AppID)` or `register_host_app_with_connection(zbus::Connection, AppID)`.
  - It returns `Ok(())` immediately when sandboxed (`/.flatpak-info` exists).
  - It builds the Registry proxy, which reads the Registry `version` property, before calling `Register`. That property read does not count as a "portal method call", and Register still succeeded.
  - The daemon must use the **same `zbus::Connection`** for Register and for every portal proxy, because registration is per D-Bus connection. The `_with_connection` variants exist for this.
- The activation token on `Activated` has no typed accessor. Read it with `activated.options().get("activation_token")` and convert it to `&str`.
- `Activated::timestamp()` returns `Duration::from_millis(raw)`. The ashpd doc comment says "since the Unix epoch", but the spec says the base is undefined. **Do not treat it as wall-clock time.** Phase A records which clock KDE actually uses.
- `configure_shortcuts` is gated on version ≥ 2 (`call_versioned`). `ConfigureShortcutsOptions::set_activation_token` exists.
- `bind_shortcuts` returns a `Request<BindShortcuts>`, and `.response()` holds the bound shortcuts with `trigger_description`. While the KDE dialog is open, the future just stays pending. The daemon must not block startup on it.

### Activation token: expectation from source

- **KDE 6.8:** xdp-kde emits `Activated(session, id, timestamp)` with empty options. The only `activation_token` handling in xdp-kde is in `ConfigureShortcuts`. The frontend passes options through unchanged (`activated_cb`). So **no `activation_token` on KDE** is expected. This is harmless for quick-translate on KDE, because the popup is a layer surface with exclusive keyboard interactivity (D3), not an xdg-activated toplevel.
- **GNOME:** xdp-gnome advertises version 2 and copies `activation-token` from gnome-shell's `AcceleratorActivated` parameters into `activation_token` when gnome-shell provides it. This is not verified here (no GNOME).

## Interactive results (KDE Plasma 6.8, 2026-10-23)

Logs: `spikes/portal-shortcuts/logs/{P-probe,A-first-bind,B-rebind,C-configure,D-restart-after-rebind,E-latency,F-cleanup}.log`.

- The user ran phases P and A on the live session: they accepted the dialog and pressed real keys.
- Phases B to F ran on the same session with no key presses. Shortcuts were fired with `org.kde.kglobalaccel.Component.invokeShortcut`, which emits the same `globalShortcutPressed` that a key press does.
- The user's rebind in phase C was simulated with `KGlobalAccel.setForeignShortcutKeys`, the call System Settings' shortcut KCM makes on Apply.

| Phase | Result |
|---|---|
| A, first bind | KDE showed the dialog once. `BindShortcuts` returned after 4.9 s, which includes the time the user took to accept. Bound and listed as `Ctrl+Alt+C`. Three real presses gave three `ACTIVATED`, each followed by `DEACTIVATED` on release. **`activation_token` ABSENT**, options empty. |
| B, re-bind in a new process | `persisted-before-bind` already lists `Ctrl+Alt+C`. **No dialog.** `BindShortcuts` took 13.8 ms. Every invoke fired. |
| C, `ConfigureShortcuts` | System Settings opened on the spike's own page (`systemsettings kcm_keys --args io.github.quicktranslate.QuickTranslate.Spike`). After the rebind to Meta+Alt+T, the running process got **`SHORTCUTS_CHANGED`** with `trigger_description="Meta+Alt+T"`, and the shortcut kept firing. |
| D, restart after the rebind | `persisted-before-bind`, `bound` and `list-after-bind` all show **Meta+Alt+T**. `BindShortcuts(preferred=CTRL+ALT+c)` did **not** reset the user's choice. kglobalaccel lists Ctrl+Alt+C as unowned, so it no longer fires. No dialog; bind took 16.5 ms. |
| E, latency | See below. |
| F, cleanup | `--cleanup`, then `KGlobalAccel.unregister(component, action)` for both components. Afterwards there was no spike or native entry in `kglobalshortcutsrc`, no kglobalaccel component, no `.desktop` file, and empty portal permission-store tables. |

### Latency

**Portal hop:** the portal adds 1.4 to 1.8 ms on real key presses, and 0.5 to 1.6 ms when fired by D-Bus. This is the time from kglobalaccel's `globalShortcutPressed` broadcast to `Activated` arriving at the app.

| Measurement | n | min | median | max |
|---|---|---|---|---|
| Real presses (phase A): kglobalaccel signal → `Activated` | 3 | 1.4 ms | 1.7 ms | 1.8 ms |
| `invokeShortcut` (phases B to E): kglobalaccel signal → `Activated` | 16 | 0.5 ms | 1.3 ms | 1.6 ms |

**Portal against native:** this comparison uses the invoke method in phase E. A script records CLOCK_MONOTONIC before each `busctl … invokeShortcut`, alternating 15 times between two shortcuts:
- a native KDE command shortcut, `qt-spike-native.desktop`, which runs `portal-shortcuts-spike --stamp`;
- the portal shortcut.

Both numbers include about 6 ms of busctl process start-up.

| Path | n | min | median | max |
|---|---|---|---|---|
| Native command shortcut → new process reaches `main()` | 15 | 30.3 ms | 35.5 ms | 40.6 ms |
| Portal shortcut → `Activated` in a running process | 15 | 7.3 ms | 8.0 ms | 8.6 ms |

kglobalaccel emits **no** `globalShortcutPressed` D-Bus signal for service (command) shortcuts. It launches them itself through KIO, which is why the native path had to be timed from the invoke. The spike's "native: kglobalaccel signal → client main()" row therefore stays at n=0.

**The portal timestamp is always 0 on KDE 6.8.** Both the portal's `Activated.timestamp` and kglobalaccel's own `globalShortcutPressed` timestamp were 0 ms on every activation, real or invoked. The value cannot be used to measure latency, which is why it is never used for that.

### What this means for quick-translate

- On KDE, the portal path is about **27 ms faster** than the compositor-binding fallback. The fallback spawns a CLI process, then forwards over D-Bus in about 3 ms. The D6 threshold (portal more than about 50 ms slower than native means recommend native) is not reached. The portal stays the default.
- **"Bind on every start" is safe.** A persisted user rebind survives `BindShortcuts` with the original preferred trigger, and no dialog repeats. D6 stays as written.
- `ShortcutsChanged` arrives in the running process, so the settings window can show the new trigger live, using the D1 `ShortcutChanged` signal.
- No activation token on KDE is confirmed, as is the release `Deactivated`. The layer-shell popup does not need the token. Only `window` mode on KDE would be affected, and it is not used there.

## Reference: the interactive walkthrough

`spikes/portal-shortcuts/run-interactive.sh` runs the same phases for a person, with real key presses. It is still the way to repeat this on another machine. Keys checked free on this machine: Ctrl+Alt+C, Ctrl+Alt+V and Meta+Alt+T. Meta+T is taken by KWin "Edit Tiles". If the dialog is **rejected** in phase A, xdp-kde stores the ID with an empty key sequence and will not prompt again. Run `--cleanup` to reset it.

### How latency is measured, and its limits

The physical key press cannot be timestamped from an unprivileged Wayland client. The spike uses three proxies, from weakest to strongest:

1. **Portal `Activated.timestamp` versus local clocks.** The value comes from kglobalaccel's `globalShortcutPressed` timestamp, which xdp-kde forwards. If it is in the CLOCK_MONOTONIC ms domain (the libinput event time), then `now_mono - ts` is key-to-arrival. If it is REALTIME taken inside kglobalaccel, it covers only the kglobalaccel-to-app part. The output reports every clock and flags the plausible ones. The evdev data in phase E settles which case applies.
2. **kglobalaccel D-Bus signal versus portal signal.** Both the spike and xdp-kde receive the same `globalShortcutPressed` broadcast from kwin. The difference between their arrival times at the spike is the cost the portal adds: xdp-kde, then frontend, then app, which is two extra D-Bus hops. This is the cost to compare against a native bind, which also starts at kglobalaccel.
3. **evdev kernel timestamps** (optional; root or `input` group needed, and the dev user has neither). This is true key-down time, CLOCK_REALTIME on the same machine, compared with the arrival time (portal) or the `main()` entry time (native `--stamp`).

The native path measured is "kglobalaccel launches a process" (KDE custom shortcut, then `--stamp` `main()`). That is what the compositor-binding fallback costs (`quick-translate --toggle` = process spawn + one D-Bus call). The D-Bus forward to the daemon, under 1 ms, is not included. Process start-up before `main()` is included in the native number, as it would be in production. Limits:

- kglobalaccel does **not** emit `globalShortcutPressed` for **service** (custom command) shortcuts (phase E). The native path is therefore timed from the D-Bus invoke, or with evdev.
- There is no control over CPU frequency or system load. Take medians over 10 or more presses.

D6 risk threshold: if the portal adds **more than about 50 ms** over native at the median, settings should recommend the native binding even where the portal exists.

## GNOME 48+: not tested (no GNOME on dev machine)

Procedure for a GNOME 48 or 49 machine. The binary builds anywhere. Run `cargo build` in `spikes/portal-shortcuts`.

1. Run `portal-shortcuts-spike --probe`. Record the GlobalShortcuts version, which should be 2 with xdp-gnome 48 or later, and whether Register and CreateSession succeed.
2. Run `portal-shortcuts-spike --bind --listen 60`. GNOME shows its own bind dialog, served by `org.gnome.Settings.GlobalShortcutsProvider`. Accept it and press Ctrl+Alt+C several times. Record `activation_token` (PRESENT is expected) and the "plausible base" of the timestamp. On GNOME 48.0, bind is known to be broken (fixed in 49). Record the exact gnome-shell, xdg-desktop-portal and xdg-desktop-portal-gnome versions.
3. Run `--bind --listen 20` again in a new process. Check that there is no dialog and that the persisted trigger is kept.
4. Rebind in GNOME Settings, restart, and repeat the phase D check.
5. Latency: kglobalaccel does not exist on GNOME. Compare a GNOME custom shortcut running `portal-shortcuts-spike --stamp` against the portal, using `sudo … --evdev-log`, with `--listen-only` or `--bind`.
6. Run `--cleanup` and remove the custom shortcut. Also remove the app's shortcut in GNOME Settings > Apps, if it is listed.

## Conclusions so far, and what would change D5/D6

- D6 step 1 (`Registry.Register` before anything else, after D9 installs the `.desktop` file) is **confirmed on KDE**. Add this to D6 and the spec: Register errors must be fatal for the portal path, because the frontend would otherwise fall back to a cgroup-derived app ID, such as the terminal's.
- The v2 assumptions in D6 hold on KDE 6.8: version = 2, so `ConfigureShortcuts` is available. But **KDE sends no `activation_token` on `Activated`** (from source; phase A confirms). D3/D4 on KDE do not need it, because they use a layer surface. The `window` shell on KDE would not get focus from a portal trigger. A sanity run showed that KWin does not focus a new token-less toplevel; see gnome-primary.md.
- Phases A to E settled the rest on KDE:
  - The portal adds 1 to 2 ms, and is about 27 ms faster than a native command bind that starts a process. The "recommend native bind" rule does not trigger on KDE.
  - The portal timestamp is always 0, so it cannot be used.
  - A persisted user rebind survives `BindShortcuts` on restart, so "bind on every start" stays.
- Still open: GNOME (all of the above), and IME and focus questions, which are covered by other spikes.
