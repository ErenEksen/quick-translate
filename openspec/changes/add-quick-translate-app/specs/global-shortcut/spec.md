# Spec Delta

## Purpose

Lets the user trigger translation from anywhere with one rebindable key combination. Where available it uses the XDG GlobalShortcuts portal. Elsewhere it uses compositor bindings that run `quick-translate --toggle`.

## ADDED Requirements

### Requirement: Default shortcut
The application SHALL request a global shortcut with id `translate-selection`, a human-readable description and a preferred trigger of `Ctrl+Alt+C`. Activating the shortcut MUST behave like `quick-translate --toggle`: show the popup when hidden, hide it when visible.

#### Scenario: Shortcut shows popup
- **WHEN** the popup is hidden and the user presses the bound shortcut
- **THEN** the popup is shown for the current selection

#### Scenario: Shortcut hides popup
- **WHEN** the popup is visible and the user presses the bound shortcut again
- **THEN** the popup is hidden

### Requirement: Runtime capability detection
On every start, the daemon SHALL detect whether the session's xdg-desktop-portal provides the GlobalShortcuts interface and which version it reports. The result MUST decide whether the portal path or the compositor-binding fallback is used. The settings window MUST show which mechanism is active.

#### Scenario: Portal available
- **WHEN** the daemon starts under KDE Plasma 6 and the portal exposes GlobalShortcuts
- **THEN** the portal path is used
- **AND** settings shows "System shortcut (portal)" as the active mechanism

#### Scenario: Portal missing
- **WHEN** the daemon starts under Sway and no GlobalShortcuts interface is available
- **THEN** the compositor-binding fallback is used
- **AND** settings shows that the shortcut must be configured in the compositor

### Requirement: Portal registration and binding
When the portal path is used, the daemon SHALL first register with the host portal Registry under the app ID `io.github.quicktranslate.QuickTranslate`, before calling any other portal method. It MUST then create a GlobalShortcuts session and bind the `translate-selection` shortcut on every daemon start, including when a binding persisted from an earlier session. Bindings that persisted from earlier sessions MUST be kept, not reset to the default.

#### Scenario: First bind on KDE
- **WHEN** the daemon starts for the first time under KDE Plasma
- **THEN** the system shows its shortcut confirmation dialog with `Ctrl+Alt+C` proposed
- **AND** after the user accepts, pressing `Ctrl+Alt+C` shows the popup

#### Scenario: Binding survives restart
- **WHEN** the user rebound the shortcut to `Super+T` and later restarts the daemon
- **THEN** after the restart, pressing `Super+T` shows the popup without the user rebinding

#### Scenario: Registry failure
- **WHEN** the host portal Registry call fails
- **THEN** the daemon logs the error and switches to the compositor-binding fallback instead of exiting

### Requirement: Displaying and changing the shortcut
On the portal path, the settings window SHALL show the trigger the portal actually assigned, which may differ from the requested default. It MUST provide a "Change shortcut" action:
- If the portal reports GlobalShortcuts interface version 2 or higher, the action MUST open the portal's configuration flow.
- Otherwise, the action MUST tell the user to change the shortcut in their system shortcut settings.

The displayed trigger MUST update when the portal reports a change.

#### Scenario: Change via portal v2
- **WHEN** the portal reports GlobalShortcuts version 2 and the user clicks "Change shortcut"
- **THEN** the system's shortcut configuration UI opens for `translate-selection`

#### Scenario: Change on an older portal
- **WHEN** the portal reports version 1 and the user clicks "Change shortcut"
- **THEN** settings shows instructions to rebind it in the system shortcut settings

#### Scenario: Shortcut changed externally
- **WHEN** the user rebinds the shortcut in system settings while the settings window is open
- **THEN** the displayed trigger updates to the new combination

### Requirement: Compositor-binding fallback
When the portal path is not available, the settings window SHALL detect the running compositor and show a ready-to-paste binding snippet that runs `quick-translate --toggle`, with a button to copy it. Snippets MUST be provided for Sway, Hyprland (native bind), niri, river, labwc, Wayfire and COSMIC, plus a generic instruction for other environments. The application MUST NOT modify compositor or desktop configuration files itself.

#### Scenario: Snippet on niri
- **WHEN** the daemon runs under niri without the GlobalShortcuts portal and the user opens settings
- **THEN** settings shows a niri `config.kdl` bind snippet that spawns `quick-translate --toggle`
- **AND** a copy button places that snippet on the clipboard

#### Scenario: Snippet references the AppImage path
- **WHEN** the app runs from an AppImage and settings shows a fallback snippet
- **THEN** the snippet invokes the AppImage path rather than a bare `quick-translate` that may not be on `PATH`

#### Scenario: Config files untouched
- **WHEN** the fallback is active
- **THEN** no file under the user's compositor configuration directory is created or modified by the application

### Requirement: Opt-in input-device fallback
The application SHALL offer an optional fallback that detects the shortcut by reading keyboard input devices directly. It MUST be disabled by default. Enabling it MUST require an explicit user action after a warning that this mode can read all keyboard input. If the user lacks read access to the input devices, the application MUST report that clearly and leave the mode inactive.

#### Scenario: Enabling shows warning
- **WHEN** the user turns on the input-device fallback in settings
- **THEN** a warning explains that the app will be able to read all keyboard input
- **AND** the mode activates only after the user confirms

#### Scenario: No device permission
- **WHEN** the user enables the fallback but cannot read the keyboard input devices
- **THEN** settings reports that permission is missing
- **AND** the mode stays inactive without crashing the daemon

### Requirement: Shortcut activation latency
The daemon SHALL begin handling a shortcut activation, either a portal activation signal or a `--toggle` request, immediately on receipt, without waiting on network or disk I/O. The selection is read and the popup shown as specified in the selection-capture and translation-popup capabilities.

#### Scenario: Activation not blocked by pending translation
- **WHEN** a translation request is in flight and the user presses the shortcut
- **THEN** the popup hides, or reshows with the new selection, without waiting for the in-flight request to finish
