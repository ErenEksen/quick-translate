# Spec Delta

## Purpose

Delivers quick-translate as a single portable AppImage and a Nix flake that run across mainstream Linux distributions. It also defines which Wayland desktop environments are supported, and at what level.

## ADDED Requirements

### Requirement: Portable AppImage
The project SHALL publish a single x86_64 AppImage that runs without installation and without root privileges. The AppImage MUST start on a system without `libfuse2` installed. It MUST also run when `APPIMAGE_EXTRACT_AND_RUN=1` is set.

#### Scenario: No libfuse2 on Ubuntu 24.04
- **WHEN** the AppImage is made executable and launched on a fresh Ubuntu 24.04 install without `libfuse2t64`
- **THEN** the application starts

#### Scenario: Extract-and-run mode
- **WHEN** the AppImage is launched with `APPIMAGE_EXTRACT_AND_RUN=1`
- **THEN** the application starts and behaves the same as a normally mounted launch

### Requirement: Distribution compatibility baseline
The AppImage SHALL run on distributions with glibc 2.35 or newer. At minimum it MUST be verified on Arch Linux, Debian 12, Ubuntu 22.04 and 24.04, Fedora 40 or newer, and openSUSE Tumbleweed.

#### Scenario: Oldest supported glibc
- **WHEN** the AppImage is launched in a Wayland session on Ubuntu 22.04 (glibc 2.35)
- **THEN** it starts without missing-symbol or missing-library errors

#### Scenario: Rolling release
- **WHEN** the AppImage is launched on current Arch Linux
- **THEN** it starts without missing-symbol or missing-library errors

### Requirement: Host system libraries not bundled
The AppImage MUST NOT bundle glibc or GPU driver libraries, including libGL, libEGL, libvulkan, libgbm and Mesa drivers. It MUST use the host's versions so that rendering works with the host's graphics drivers.

#### Scenario: Bundle content check
- **WHEN** the AppImage contents are listed
- **THEN** none of `libc.so.6`, `libGL.so*`, `libEGL.so*`, `libvulkan.so*`, `libgbm.so*` or Mesa driver files are present

#### Scenario: Proprietary GPU driver host
- **WHEN** the AppImage runs on a host using a vendor GPU driver
- **THEN** the popup renders using the host driver, or a software fallback, without crashing

### Requirement: Nix flake
The repository SHALL provide a Nix flake that exposes `packages.<system>.default` and `apps.<system>.default` for at least `x86_64-linux`. The package MUST build from source with `nix build` and run with `nix run`. The documentation MUST also explain how to run the AppImage on NixOS through `appimage-run` or AppImage binfmt support.

#### Scenario: nix run
- **WHEN** a NixOS user in a Wayland session runs `nix run github:<owner>/quick-translate`
- **THEN** the application starts

#### Scenario: nix build
- **WHEN** a user runs `nix build` in the repository
- **THEN** `result/bin/quick-translate` exists and is executable

### Requirement: Supported environment matrix
The project SHALL document and honor this support matrix:
- **Full support:** popup at the cursor, background selection read, and either a portal shortcut or a documented compositor bind. Covers KDE Plasma 6, Hyprland, Sway, niri, labwc, Wayfire, river and COSMIC.
- **Reduced support:** a centered popup, a portal shortcut, and a selection read on popup focus. Covers GNOME 48 and newer.
- **Best effort:** environments whose Wayland sessions are still maturing. Covers Cinnamon, XFCE and MATE.

X11 sessions MUST be documented as unsupported.

#### Scenario: Documentation lists the matrix
- **WHEN** a user reads the project README
- **THEN** it lists each environment with its support level and states that X11 is not supported

#### Scenario: Unsupported session refused
- **WHEN** the AppImage is started in an X11 session
- **THEN** it exits with a message stating that only Wayland sessions are supported
