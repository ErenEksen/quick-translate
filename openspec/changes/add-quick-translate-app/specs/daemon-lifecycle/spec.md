# Spec Delta

## Purpose

Keeps a single resident quick-translate process running in the user's Wayland session. It exposes a command-line interface to control that process and installs the desktop integration the system needs to identify and autostart the app.

## ADDED Requirements

### Requirement: Wayland session required
The application SHALL run only inside a Wayland session. If `WAYLAND_DISPLAY` is unset or the Wayland connection fails, the application MUST print a clear error message stating that a Wayland session is required and exit with a non-zero status. The application MUST NOT fall back to X11.

#### Scenario: Started in an X11-only session
- **WHEN** the user runs `quick-translate` in a session where `WAYLAND_DISPLAY` is unset
- **THEN** the process prints an error message stating that a Wayland session is required
- **AND** exits with a non-zero status without opening any window

#### Scenario: Wayland socket unreachable
- **WHEN** `WAYLAND_DISPLAY` is set but connecting to the compositor fails
- **THEN** the process prints an error naming the socket it tried and exits with a non-zero status

### Requirement: Single instance per session
The application SHALL run at most one daemon per user session, identified by owning the D-Bus well-known name `io.github.quicktranslate.QuickTranslate` on the session bus. A later invocation that finds the name owned MUST forward its command to the running daemon and exit within 200 ms.

#### Scenario: Second invocation forwards its command
- **WHEN** the daemon is running and the user runs `quick-translate --settings`
- **THEN** the running daemon opens the settings window
- **AND** the invoking process exits with status 0 within 200 ms without starting a second daemon

#### Scenario: Two instances started simultaneously
- **WHEN** two `quick-translate --daemon` processes start at the same moment
- **THEN** exactly one becomes the daemon
- **AND** the other exits with status 0

### Requirement: Command-line interface
The application SHALL accept these commands:
- no arguments: start the daemon if it is not running, otherwise open the settings window
- `--toggle`: show the popup for the current selection, or hide it if it is visible
- `--show`: show the popup
- `--settings`: open the settings window
- `--quit`: stop the daemon
- `--daemon`: start the daemon in the background without showing any UI
- `--remove-integration`: remove the installed desktop integration
- `--status`: print whether a daemon is running and the detected capabilities (layer-shell, data-control, portal shortcuts and their versions)
- `--version`: print the version and exit

If no daemon is running, `--toggle` and `--show` MUST start the daemon and then perform the action.

#### Scenario: Toggle with no daemon running
- **WHEN** no daemon is running and the user runs `quick-translate --toggle`
- **THEN** a daemon starts
- **AND** the popup is shown for the current selection

#### Scenario: Toggle while popup is visible
- **WHEN** the popup is visible and `quick-translate --toggle` is invoked
- **THEN** the popup is hidden and the daemon keeps running

#### Scenario: Quit
- **WHEN** the daemon is running and the user runs `quick-translate --quit`
- **THEN** the daemon releases its D-Bus name, unregisters its shortcut session and exits with status 0

#### Scenario: Unknown argument
- **WHEN** the user runs `quick-translate --bogus`
- **THEN** the process prints usage help and exits with a non-zero status without contacting the daemon

### Requirement: Warm resident daemon
While idle, the daemon SHALL keep the resources needed to show the popup ready: the Wayland connection, renderer, loaded fonts and HTTP client. This lets the popup appear without cold-start cost. When idle with the popup hidden, the daemon MUST use effectively no CPU, with no periodic wakeups for polling. Hiding the popup MUST NOT terminate the daemon.

#### Scenario: Idle daemon uses no CPU
- **WHEN** the daemon has been idle with the popup hidden for 60 seconds
- **THEN** its measured CPU usage over the next 60 seconds averages below 0.1% of one core

#### Scenario: Popup hidden, daemon persists
- **WHEN** the user dismisses the popup
- **THEN** the daemon process keeps running and owns its D-Bus name

### Requirement: Configuration storage
The application SHALL store non-secret settings in `$XDG_CONFIG_HOME/quick-translate/config.toml`, defaulting to `~/.config/quick-translate/config.toml` when `XDG_CONFIG_HOME` is unset. Secrets such as API keys MUST NOT be written to this file or to any other file. A missing config file MUST result in defaults. A malformed config file MUST NOT crash the daemon: it MUST fall back to defaults, report the parse error to the user, and leave the malformed file unmodified.

#### Scenario: First run without config
- **WHEN** the daemon starts and no config file exists
- **THEN** it runs with default settings: the DeepL backend, the default shortcut and autostart enabled

#### Scenario: Malformed config
- **WHEN** the config file contains invalid TOML
- **THEN** the daemon starts with defaults
- **AND** shows a notice that names the file and the parse error
- **AND** does not overwrite the file until the user saves settings

#### Scenario: Secrets never in config
- **WHEN** the user saves an API key in settings
- **THEN** the config file contains no API key material

### Requirement: First-run desktop integration
On first start, and on every later start whenever the installed files are missing or outdated, the daemon SHALL install:
- a `.desktop` file named `io.github.quicktranslate.QuickTranslate.desktop` in `$XDG_DATA_HOME/applications`
- the application icon in `$XDG_DATA_HOME/icons`
- an XDG autostart entry in `$XDG_CONFIG_HOME/autostart` that runs the app with `--daemon`

When the app runs from an AppImage, `Exec` lines MUST point to the AppImage path given by `$APPIMAGE`. They MUST be rewritten when that path changes.

#### Scenario: First run from an AppImage
- **WHEN** the AppImage at `~/Apps/quick-translate.AppImage` is started for the first time
- **THEN** the `.desktop` file, icon and autostart entry exist
- **AND** their `Exec` lines reference `~/Apps/quick-translate.AppImage`

#### Scenario: AppImage moved
- **WHEN** the user moves the AppImage to `~/bin/qt.AppImage` and starts it from there
- **THEN** the installed `.desktop` and autostart `Exec` lines are updated to `~/bin/qt.AppImage`

#### Scenario: Autostart disabled by user
- **WHEN** the user turns off "Start on login" in settings
- **THEN** the autostart entry is removed
- **AND** it is not recreated on the next daemon start

### Requirement: Removing desktop integration
`quick-translate --remove-integration` SHALL remove the installed `.desktop` file, icon and autostart entry. It MUST stop a running daemon. It MUST leave the config file and stored secrets untouched unless the user explicitly asks to remove them.

#### Scenario: Remove integration
- **WHEN** the user runs `quick-translate --remove-integration`
- **THEN** the `.desktop` file, icon and autostart entry no longer exist
- **AND** any running daemon has exited
- **AND** `config.toml` still exists
