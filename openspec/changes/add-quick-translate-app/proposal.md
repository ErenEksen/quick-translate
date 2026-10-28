# Proposal

## Why

Linux has no DeepL-desktop-style "select text, press a hotkey, see the translation next to the cursor" tool that works properly on Wayland. DeepL ships no Linux app. Existing tools (Crow Translate, Pot) can't place a popup at the cursor or read the selection reliably under Wayland's security model. Wayland now has the building blocks to do this well: the GlobalShortcuts portal, ext-data-control, wlr-layer-shell and xdg-activation tokens in portal signals. This change builds the whole application from an empty repository.

## What Changes

- New Rust application `quick-translate`, Wayland-only (no X11 code paths), shipped as a single AppImage plus a Nix flake.
- A resident background daemon keeps the UI, fonts, GPU/renderer and HTTP client warm, so the popup appears without cold-start cost. A CLI (`quick-translate --toggle` and friends) talks to the daemon over D-Bus and enforces a single instance.
- Global shortcut, default `Ctrl+Alt+C`:
  - Registered through the XDG GlobalShortcuts portal where available (KDE Plasma, GNOME 48+, Hyprland), so users rebind it from their system's shortcut settings or from the app's "Change shortcut" button.
  - Elsewhere (Sway, niri, river, labwc, Wayfire, COSMIC, and others), the app shows a ready-to-paste compositor binding that runs `quick-translate --toggle`.
- Selection capture, in order:
  1. Read the PRIMARY selection in the background through ext-data-control-v1 or wlr-data-control v2.
  2. Otherwise read it on popup focus, with focus legitimized by an xdg-activation token.
  3. Otherwise prefill from CLIPBOARD, or open an empty, editable source pane.
- Translation popup, split into the original text on the left and the translation on the right:
  - Appears at the mouse cursor on layer-shell compositors and is resizable.
  - Appears as a centered normal window on GNOME, where Mutter has no layer-shell.
  - `Ctrl+C` or the Copy button copies the translation to the clipboard and closes the popup. `Esc` closes it without copying. The app never types into or modifies the source application.
- Translation backends:
  - **DeepL** is the default and uses a user-supplied API key. The settings window has a "Get API key" button and shows remaining quota.
  - **OpenAI-compatible AI translation** uses a user-supplied base URL, model and API key, and streams tokens into the translation pane.
  - Secrets are stored in the desktop keyring through the Secret Service API.
  - No other backends and no offline models.
- First-run desktop integration: installs the `.desktop` file, icon and XDG autostart entry, which the portal needs to identify an unsandboxed app.
- A settings window for backend selection and credentials, target language, shortcut, and autostart.

## Capabilities

### New Capabilities
- `daemon-lifecycle`: resident daemon, single instance over D-Bus, CLI commands (`--toggle`, `--show`, `--settings`, `--quit`), first-run desktop integration (`.desktop` file, icon, autostart) and clean shutdown.
- `global-shortcut`: portal-based shortcut registration and rebinding, runtime capability detection, compositor-binding fallback guidance, and opt-in evdev fallback.
- `selection-capture`: obtaining the text to translate through data-control, the focus-time PRIMARY read and the clipboard/empty-pane fallback, with runtime protocol detection.
- `translation-popup`: popup placement at the cursor (layer-shell probe) or centered (GNOME), two-pane layout, user resizing, keyboard handling, copy-and-close, dismissal, and language controls.
- `translation-backends`: DeepL and OpenAI-compatible backends, backend selection, keyring credential storage, streaming, quota display, and error reporting.
- `distribution`: AppImage built against an old glibc baseline with the static type2 runtime (no libfuse2 needed), a Nix flake, and the supported-environment matrix.

### Modified Capabilities
<!-- None: greenfield project, no existing specs. -->

## Impact

- **Code**: new Cargo workspace (daemon/UI binary plus internal crates for platform, selection, backends and IPC). The repository is currently empty apart from `openspec/`.
- **Runtime dependencies on the host**: a Wayland compositor, an xdg-desktop-portal with a desktop backend (optional, only needed for portal shortcuts), D-Bus session bus, Secret Service provider (KWallet or GNOME Keyring), and GPU drivers or software rendering.
- **Key crates (planned)**: `smithay-client-toolkit`/`wayland-client` with the wlr/ext protocols, a GUI layer (libcosmic or iced, chosen by spike), `ashpd` (portals), `zbus` (IPC), `wl-clipboard-rs` / `smithay-clipboard`, `reqwest` (rustls), `oo7` or `keyring` (secrets), `serde`/`toml` (config).
- **External services**: DeepL API (`api.deepl.com` / `api-free.deepl.com`) and user-chosen OpenAI-compatible endpoints. Text the user selects is sent to the chosen service.
- **Supported environments**:
  - Full: KDE Plasma 6, Hyprland, Sway, niri, labwc, Wayfire, river, COSMIC.
  - Reduced: GNOME, where the popup is centered.
  - Best-effort as their Wayland sessions mature: Cinnamon, XFCE, MATE. X11 sessions are explicitly unsupported.
