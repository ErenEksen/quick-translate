# Spike 2.5: focus-time PRIMARY read (GNOME path of D5)

Status: **not tested on GNOME (no GNOME on dev machine).** The spike binary is built and ready. One sanity run happened on KDE Plasma 6.8: the window mapped, and the no-token path behaved as expected. The click-focus and portal-trigger runs on KDE are **PENDING: needs user**. Both GNOME paths are **PENDING: needs a GNOME 48+ machine**.

Code: `spikes/gnome-primary/` (standalone crate, own `[workspace]`).
Binary: `target/agent-portal-spike/debug/gnome-primary-spike`.

## What the spike does

The spike uses the same stack iced 0.14 uses on Wayland:

- **winit 0.30.13**, the winit version iced_winit 0.14.1 depends on
- **smithay-clipboard 0.7.3**, which is where iced's `clipboard::Kind::Primary` ends up: `iced_winit::Clipboard` → `window_clipboard 0.5.1::read_primary` → `clipboard_wayland 0.2.2` → `smithay_clipboard::Clipboard::load_primary`
- **softbuffer**, used only to attach a buffer so the toplevel maps

The run goes like this:

1. It reads the token from `--token` or `$XDG_ACTIVATION_TOKEN`, then removes the variable from its environment. It also reports `DESKTOP_STARTUP_ID`.
2. It creates a 420×140 toplevel with `app_id = io.github.quicktranslate.QuickTranslate.PrimarySpike` and `WindowAttributesExtStartupNotify::with_activation_token`. winit calls `xdg_activation_v1.activate(token, surface)` before the first commit.
3. Like iced, it creates one smithay-clipboard per window from that window's `wl_display`.
4. On `WindowEvent::Focused(true)` it calls `load_primary()` on a helper thread and retries every 2 ms for up to `--budget-ms` (default 500). It records the number of attempts, the ms from focus to text, and each distinct error.
5. Timeline lines are printed relative to process start (or to `Activated` receipt in portal mode). `--redact` prints only the character count. `--clipboard` also reads CLIPBOARD.
6. A hard per-window timeout applies (`--timeout`, default 3 s). The exit code is 0 when PRIMARY was read, 2 when keyboard focus was not granted, and 3 when the window had focus but PRIMARY could not be read.

There are two modes:

- **default**: the "GNOME custom shortcut runs the CLI" path.
- **`--portal`**: the "portal trigger" path, done in-process as the daemon would. Register, CreateSession and BindShortcuts run on a tokio thread. Each `Activated` opens a window using `options["activation_token"]`. `--count N` and `--listen S` bound the run, and `--trigger` changes the preferred trigger. `--cleanup` removes the spike's `.desktop` file.

## Sanity run on KDE Plasma 6.8 (KWin 6.8.0)

The binary was launched by the agent from a background shell, not from a focused terminal, with no token:

```
[env] XDG_CURRENT_DESKTOP=Some("KDE") WAYLAND_DISPLAY=Some("wayland-1") XDG_ACTIVATION_TOKEN=ABSENT DESKTOP_STARTUP_ID=ABSENT
[t+   10.3ms] window #1 created, activation token ABSENT (no activate request)
[t+   12.0ms] first frame presented (0.5ms after create_window)
[t+ 3012.0ms] TIMEOUT: keyboard focus NOT granted within 3s (first frame presented: true)
exit=2
```

- winit and softbuffer map a toplevel about 12 ms after process start.
- **KWin does not give keyboard focus to a token-less new toplevel**, because of focus-stealing prevention. Without focus, no PRIMARY offer is delivered. The spike then times out cleanly, which is the spec's "Focus not granted" fallthrough.
- The PRIMARY read itself was **not** exercised. That needs focus, and focus needs either a user click or a valid token. The user's selection was never written or modified.

## Findings from reading the toolkit code

Each of these affects the implementation of D5:

1. **`load_primary()` has no timeout.** `smithay_clipboard::Clipboard::load_primary` sends a command to its worker thread and then calls a plain `mpsc::Receiver::recv()`. It returns only after the source client writes the data and closes the pipe. A source that never answers blocks the caller indefinitely. iced calls it synchronously inside `iced_winit::Clipboard::read`, behind a `Mutex`, so the UI thread would block. **The daemon must not read through iced's `clipboard::read_primary` on the UI thread.** Either call smithay-clipboard on a helper thread with `recv_timeout(150 ms)`, as the spike does, or use its own zwp-primary-selection reader. Either way the read meets the 150 ms capture budget.
2. **Focus gate and seat gate.** smithay-clipboard returns errors in two cases:
   - `client doesn't have focus` until **its own** `wl_keyboard` sees `enter`
   - `no events received on any seat` until it sees a `wl_keyboard.key` or `modifiers` event, or a pointer button event

   Compositors must send `modifiers` right after `enter`, and the trigger modifiers (Ctrl+Alt) are usually still held. So both conditions should clear within one roundtrip of focus. But the clipboard's worker thread and winit's queue are separate, so `Focused(true)` can arrive before the worker has processed `enter` and the selection event. **Retry for a few ms after focus; do not do a single read.** The spike reports how many attempts were needed.
3. **MIME handling matches the spec.** The order is `text/plain;charset=utf-8`, then `UTF8_STRING`, with `text/plain` only as a fallback. Invalid UTF-8 is decoded lossily (`String::from_utf8_lossy`), and CRLF is normalized to LF for `text/plain*`. Trimming and the 10 000-character limit remain the app's job.
4. **Token timing.** winit sends `activate` before the surface's first commit, so before it maps. Mutter has to honour an activation that arrives for a not-yet-mapped surface. Whether it does is part of what the GNOME runs must show. If it does not, the fix is to activate after the first configure. winit 0.30 has no API for that with a foreign token, so it would mean a raw `xdg_activation_v1` call through the raw handles.
5. **The KDE portal sends no `activation_token`** (see portal-shortcuts.md). The `window` shell therefore cannot rely on portal-triggered focus on KDE. This does not matter, because D3 uses layer-shell with exclusive keyboard interactivity on KDE.

## PENDING: needs user (KDE, optional, about 2 minutes)

Use `spikes/portal-shortcuts/run-interactive.sh G`. It runs before phase F, after phase A has bound the shortcut.

- **G1:** select text in another app, run `gnome-primary-spike --timeout 10 --redact`, and **click the window** within 10 s. The expected output is `Focused(true)`, then `PRIMARY OK: attempt n, x ms after focus, N chars`. This verifies the focus-time read through iced's clipboard stack on KWin.
- **G2:** run `portal-shortcuts-spike --bind --listen 30 --exec "gnome-primary-spike --timeout 3 --redact"`, select text, and press the portal shortcut. Expect `activation_token=ABSENT`, then most likely `TIMEOUT` with exit 2. Record whether KWin focuses the window anyway.

## PENDING: GNOME 48+ procedure (not tested; no GNOME on dev machine)

Prerequisites:

- GNOME 48 or 49 on Wayland
- Build with `cd spikes/gnome-primary && cargo build`, or copy the binary. Wayland is dlopen'ed, so there are no build-time system libraries.
- A text source that sets PRIMARY when you select text: GNOME Text Editor, gedit or Firefox
- Record `gnome-shell --version`, `pacman -Q`/`rpm -q` for xdg-desktop-portal and xdg-desktop-portal-gnome, and `mutter`

**Path 1: GNOME custom shortcut launches the CLI.**

1. Open Settings > Keyboard > View and Customize Shortcuts > Custom Shortcuts > Add.
   - Name: `PRIMARY spike`
   - Command: `sh -c '/ABS/PATH/gnome-primary-spike --timeout 3 >> $HOME/gnome-primary.log 2>&1'`
   - Shortcut: Ctrl+Alt+P

   The command field has no shell redirection, hence the `sh -c`.
2. Select a word in Text Editor and press Ctrl+Alt+P. Repeat five times.
3. In `~/gnome-primary.log`, record:
   - whether `XDG_ACTIVATION_TOKEN` and `DESKTOP_STARTUP_ID` are PRESENT. This answers whether gsd-media-keys or gnome-shell passes a token to custom shortcut commands.
   - whether `Focused(true)` arrives, or whether you get `TIMEOUT` with exit 2. Mutter usually shows a "… is ready" notification instead of focusing.
   - for `PRIMARY OK`, the ms after focus, and whether the text matches the selection
4. Control run: start `gnome-primary-spike --timeout 5` from a GNOME Terminal. A focused terminal gives no token, so the expected result is `TIMEOUT`, unless Mutter focuses it anyway.

**Path 2: portal trigger (as the daemon does it).**

1. Run `gnome-primary-spike --portal --count 5 --listen 120` in a terminal.
2. Accept GNOME's shortcut dialog, which proposes Ctrl+Alt+C. If it does not appear, or the bind fails (a known GNOME 48.0 problem, fixed in 49), record the exact error and versions.
3. Select text in Text Editor and press Ctrl+Alt+C. Pressing it again while a window is open closes it (toggle). Do this five times.
4. Record:
   - `activation_token=PRESENT(len=…)` on `ACTIVATED` (expected PRESENT on GNOME with portal v2)
   - `window #n created, activation token PASSED`
   - `Focused(true)` latency
   - the `PRIMARY OK` attempts and ms after focus
   - the text match with `--redact` off
5. Variation: select text in a different app, such as Firefox, which also exercises the `UTF8_STRING`/`text/plain` offers.
6. Clean up: run `gnome-primary-spike --cleanup`, remove the custom shortcut, and remove the app's global shortcut in Settings if it is listed.

Pass criteria for task 2.5:

- On the portal path, the selected text is returned in at least four out of five tries, within 150 ms of focus, with the token present.
- On the CLI path, record whatever happens. If focus is denied, that confirms the Risks entry, "custom shortcut launching the CLI without a token", and the spec's clipboard/empty-pane fallback is the correct behaviour there.

## Conclusions so far, and what would change D5

- The D5 mechanism is implementable with the iced 0.14 stack: map with the token, wait for `Focused(true)`, then read PRIMARY. **However, the read must go off the UI thread with its own 150 ms timeout and a short retry loop.** iced's clipboard API blocks without a timeout. Proposed D5 amendment: "the focus-time read uses a dedicated primary-selection reader (smithay-clipboard on a helper thread, or a small zwp-primary-selection client on the window's connection) bounded by the 150 ms capture budget, retried until the budget expires because the selection offer can lag the focus event."
- If GNOME path 2 shows that Mutter ignores `activate` before the first commit, D5 must say "activate after the first configure". That needs raw xdg-activation access, not winit's attribute.
- If GNOME path 1 shows no token, which is likely, document that on GNOME **the custom-shortcut CLI path falls back to CLIPBOARD or an empty pane**, and recommend the portal in settings. This is already covered by the Risks entry, so no design change is needed.
- If GNOME path 2 fails even with a token, the GNOME selection story reduces to CLIPBOARD and an empty pane. D5's no-data-control branch would then only be useful where focus is granted, and the proposal's GNOME claims need adjusting in task 2.6.
