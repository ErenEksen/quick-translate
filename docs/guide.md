# Quick Translate: full guide

The short overview is in the [README](../README.md).


Select text in any application, press a shortcut, and read the translation in a popup at the
mouse cursor. Quick Translate is a small resident daemon for **Wayland** desktops. It reads the
clipboard (falling back to the current selection), shows the source text and its
translation side by side, and closes on Esc. Ctrl+C copies the translation and closes the
popup. Translations come from **DeepL** (the default) or any **OpenAI-compatible** chat
completions endpoint, such as OpenAI, OpenRouter, Ollama or LM Studio.

**Privacy.** The selected text is sent only to the backend you configured. There is no
telemetry. The selected and translated text stays in memory: it is never written to disk, to
the config file or to logs (logs record lengths only). API keys live in the desktop keyring
(Secret Service), never in a file.

## Install

### AppImage (any distribution)

Download `quick-translate-<version>-x86_64.AppImage` from the
[releases page](https://github.com/ErenEksen/quick-translate/releases), then:

```sh
chmod +x quick-translate-*-x86_64.AppImage
./quick-translate-*-x86_64.AppImage
```

No `libfuse2` is needed: the AppImage uses the static type 2 runtime. If FUSE is unavailable
(some containers), run it with `APPIMAGE_EXTRACT_AND_RUN=1`. It needs glibc 2.35 or newer
(Ubuntu 22.04, Debian 12, Fedora 40, openSUSE Tumbleweed, Arch and later). The host's own
Wayland, xkbcommon and GPU libraries are used.

Keep the AppImage where you want it to stay, for example in `~/Applications`. The desktop
entry and the autostart entry point at its path. If you move it, start it once from the new
location and the entries are rewritten.

### NixOS: running the AppImage

NixOS cannot run AppImages directly. Either enable AppImage support (NixOS 24.05 or later) in
`configuration.nix`:

```nix
programs.appimage = {
  enable = true;
  binfmt = true;
};
```

After `nixos-rebuild switch`, the AppImage runs as above. Or use `appimage-run` without
changing the system configuration:

```sh
nix run nixpkgs#appimage-run -- ./quick-translate-*-x86_64.AppImage
```

### Nix flake

```sh
nix run github:ErenEksen/quick-translate             # run without installing
nix profile install github:ErenEksen/quick-translate # install into your profile
```

The flake exposes `packages.x86_64-linux.default` and `apps.x86_64-linux.default`, and
`aarch64-linux` too. Inside a checkout, `nix build` produces `result/bin/quick-translate`.

When it runs from the flake, the installed launcher and autostart entries point at the binary
in the Nix store. After an upgrade, start `quick-translate` once so they are rewritten before
the old store path is garbage-collected.

## First run

Start Quick Translate once, from your application launcher or a terminal (`quick-translate`
or the AppImage). The daemon starts in the background and installs:

- `$XDG_DATA_HOME/applications/io.github.quicktranslate.QuickTranslate.desktop` (opens the
  settings window)
- the icon in `$XDG_DATA_HOME/icons/hicolor/scalable/apps/`
- `$XDG_CONFIG_HOME/autostart/io.github.quicktranslate.QuickTranslate.desktop`, which runs
  `quick-translate --daemon` on login

`$XDG_DATA_HOME` defaults to `~/.local/share` and `$XDG_CONFIG_HOME` to `~/.config`.

Then open settings (run `quick-translate` again, or use the launcher entry) and set up a
backend:

- **DeepL:** click **Get API key**, paste the key and save it. Settings checks the key and
  shows your character usage.
- **OpenAI-compatible:** enter the base URL, the model and, unless it is a local server, an
  API key. Then click **Test connection**.

[docs/backends.md](backends.md) covers both backends, including free and Pro DeepL keys
and example endpoints.

## Shortcut

The default shortcut is **Ctrl+Alt+C**. It toggles the popup: press it once to show it, and
again to hide it.

- **Desktops with the GlobalShortcuts portal** (KDE Plasma 6, GNOME 48+, Hyprland with
  xdg-desktop-portal-hyprland): Quick Translate registers the shortcut with the portal. The
  desktop asks you to confirm it on first start. Settings shows the key that was actually
  assigned and has a **Change shortcut** button.
- **Other compositors** (Sway, niri, river, labwc, Wayfire, COSMIC, ...): bind a key in your
  compositor to run `quick-translate --toggle`. Settings shows a ready-to-paste snippet for
  your compositor with the right path, for example for Sway:

  ```
  bindsym Ctrl+Alt+c exec quick-translate --toggle
  ```

  Quick Translate never edits compositor configuration files.

[docs/shortcuts.md](shortcuts.md) has the setup for each environment, and describes the
opt-in input-device fallback for setups where neither option works.

## Command-line usage

```
quick-translate [COMMAND]
```

Only one command is accepted per invocation.

| Command | What it does |
|---|---|
| *(none)* | Start the daemon if it is not running, otherwise open the settings window. |
| `--toggle` | Show the popup for the current selection, or hide it if it is visible. Bind your shortcut to this. |
| `--show` | Show the popup for the current selection. |
| `--settings` | Open the settings window. |
| `--quit` | Stop the daemon. |
| `--daemon` | Run the daemon without showing any UI (used by the autostart entry). |
| `--remove-integration` | Stop the daemon and remove the `.desktop` file, icon and autostart entry. |
| `--status` | Print whether the daemon is running and the detected capabilities. |
| `--version`, `-V` | Print the version and exit. |
| `--help`, `-h` | Print the usage and exit. |

`--toggle`, `--show` and `--settings` start the daemon first when it is not running. An
unknown argument prints the usage and exits with status 2. Without a Wayland session
(`WAYLAND_DISPLAY` unset) the commands that need one fail with:

```
quick-translate requires a Wayland session: WAYLAND_DISPLAY is not set (X11 is not supported)
```

## Lifecycle

- **One daemon per session.** The daemon owns the D-Bus name
  `io.github.quicktranslate.QuickTranslate` on the session bus. Later invocations forward
  their command to it and exit right away. A second `--daemon` exits with status 0. Without a
  session bus, a socket in `$XDG_RUNTIME_DIR` is used instead.
- **Resident and idle.** The daemon keeps the Wayland connection, renderer, fonts and HTTP
  client warm, so the popup appears without start-up cost. Hiding the popup does not stop the
  daemon. When idle it does no polling.
- **Autostart.** The autostart entry runs `quick-translate --daemon` on login. Turn it off
  with **Start on login** in settings. The entry is then removed and not recreated on later
  starts. The `.desktop` file, icon and autostart entry are rewritten on start when they are
  missing or outdated, for example after the AppImage was moved.
- **Configuration.** Non-secret settings are stored in
  `$XDG_CONFIG_HOME/quick-translate/config.toml` (default
  `~/.config/quick-translate/config.toml`). The file is created when you first save settings.
  A malformed file is not overwritten: the daemon runs with defaults and tells you about the
  parse error. API keys are stored in the keyring, never in this file.
- **Removal.** `quick-translate --remove-integration` stops the daemon and removes the
  `.desktop` file, icon and autostart entry. It keeps `config.toml` and the keys in your
  keyring. To remove everything, also delete the AppImage (or the Nix profile entry), the
  config directory and the keyring items:

  ```sh
  quick-translate --remove-integration
  rm -r ~/.config/quick-translate
  secret-tool clear application io.github.quicktranslate.QuickTranslate
  ```

  `secret-tool` is in the `libsecret` package. You can also delete the items in your keyring
  manager (Seahorse, KWalletManager, KeePassXC).

## Supported environments

Wayland only. **X11 sessions are not supported**: Quick Translate exits with an error message
instead of falling back to X11.

| Level | Environments | What you get |
|---|---|---|
| **Full** | KDE Plasma 6, Hyprland, Sway, niri, labwc, Wayfire, river, COSMIC | Popup at the mouse cursor, selection read in the background, and either a portal shortcut or a documented compositor binding. |
| **Reduced** | GNOME 48 and later | A popup window centered by GNOME, a portal shortcut, and the selection read when the popup gets keyboard focus. GNOME offers no layer-shell or data-control protocols, so cursor placement and background reads are not possible. |
| **Best effort** | Cinnamon, XFCE, MATE (Wayland sessions) | Their Wayland sessions are still maturing. Quick Translate uses whatever the compositor offers. |
| **Unsupported** | Any X11 session | Exits with "requires a Wayland session". |

`quick-translate --status` shows what your session provides: layer-shell, data-control
(ext or wlr), xdg-activation, fractional scaling, the GlobalShortcuts portal version, and
which popup shell and shortcut mechanism the daemon uses.

## Troubleshooting

- **Check what was detected:** `quick-translate --status`. With the daemon running it
  also shows the popup shell and the active shortcut mechanism.
- **Debug logs:** stop the daemon and run it in a terminal:

  ```sh
  quick-translate --quit
  QT_LOG=debug quick-translate --daemon
  ```

  A bare level such as `debug` applies to Quick Translate only. A full filter such as
  `QT_LOG=warn,quick_translate_ui=trace` also works. Logs never contain the selected or
  translated text.
- **Rendering problems:** the popup uses the CPU renderer (tiny-skia) by default because it
  uses far less memory when idle. To try the GPU renderer, run the daemon with
  `ICED_BACKEND=wgpu quick-translate --daemon`.
- **Nothing happens on the shortcut:** see [docs/shortcuts.md](shortcuts.md). Run
  `quick-translate --toggle` in a terminal. If that works, the shortcut binding is the
  problem.
- **Popup shows "not configured":** set up the backend in settings
  ([docs/backends.md](backends.md)).
- **Keys are forgotten after a restart:** no Secret Service provider (GNOME Keyring,
  KWallet 6, KeePassXC) is running, so keys are kept for the session only.

## Building from source

Requirements: Rust 1.90 or newer, `pkg-config`, and the Wayland and xkbcommon development
files (Arch: `pacman -S --needed rust pkgconf wayland libxkbcommon`; Debian/Ubuntu:
`apt install pkg-config libwayland-dev libxkbcommon-dev`).

```sh
cargo build --release -p quick-translate
./target/release/quick-translate --version
```

Tests: `cargo test --workspace`. Some integration tests need a headless Sway
(`scripts/headless-sway.sh`); [docs/testing.md](testing.md) describes the test setup and
the manual checks.

AppImage (built in an `ubuntu:22.04` container with podman or docker, output in `dist/`):

```sh
packaging/appimage/build.sh
```

Nix:

```sh
nix build                     # result/bin/quick-translate
nix run . -- --version
nix develop -c cargo build    # dev shell with the toolchain and test tools
```

Without Nix installed, `packaging/nix/check-in-container.sh` runs these in the
`nixos/nix` container image.

## License

Licensed under either of the MIT license or the Apache License 2.0, at your option.
