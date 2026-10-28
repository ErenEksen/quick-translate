# Spec Delta

## Purpose

Presents the source text and its translation side by side in a fast, resizable popup placed at the mouse cursor where the compositor allows it, with keyboard-driven copy-and-close behavior.

## ADDED Requirements

### Requirement: Two-pane layout
The popup SHALL show the source text in an editable left pane and the translation in a read-only, selectable right pane, separated by a draggable splitter. The header SHALL contain a source-language selector (default "Auto-detect", showing the detected language when known), a swap-languages button, a target-language selector, a Copy button, and an indicator of the active backend.

#### Scenario: Popup content
- **WHEN** the user triggers translation of "Merhaba dünya" with target English
- **THEN** the left pane shows "Merhaba dünya", the right pane shows the translation, and the header shows the detected source language and the active backend

#### Scenario: Splitter drag
- **WHEN** the user drags the splitter to the right
- **THEN** the left pane widens and the right pane narrows accordingly

### Requirement: Editable source with debounced re-translation
Edits to the source pane SHALL trigger a new translation 400 ms after the user stops typing. A pending or in-flight translation for superseded text MUST be cancelled.

#### Scenario: User edits source
- **WHEN** the user changes the source text and pauses typing for 400 ms
- **THEN** a new translation of the edited text replaces the previous translation

#### Scenario: Continuous typing
- **WHEN** the user types continuously with gaps under 400 ms
- **THEN** no translation request is sent until typing pauses

### Requirement: Language selection
The target language SHALL default to the language of the system locale (from `LC_MESSAGES`, falling back to `LANG`), or English if the locale language is unsupported by the active backend. The last chosen target language MUST be remembered across popups and restarts. The swap button SHALL exchange source and target languages, and SHALL move the current translation into the source pane. When the source is "Auto-detect", the detected language SHALL be used as the new target.

#### Scenario: Default from locale
- **WHEN** the app runs for the first time with `LANG=tr_TR.UTF-8`
- **THEN** the target language is Turkish

#### Scenario: Remembered choice
- **WHEN** the user selects German as target and later opens a new popup after a restart
- **THEN** the target language is German

#### Scenario: Swap
- **WHEN** the source is auto-detected as English, the target is Turkish, and the user presses swap
- **THEN** the source becomes Turkish with the previous translation as source text, the target becomes English, and a new translation starts

### Requirement: Placement at cursor on layer-shell compositors
On compositors that advertise wlr-layer-shell, the popup SHALL appear with its top-left corner near the mouse cursor, offset by a small fixed distance, on the output that currently contains the pointer. The popup MUST be clamped so it is fully visible on that output. Placement MUST be correct with fractional scaling and multi-monitor layouts. If the cursor position cannot be determined within 50 ms, the popup SHALL be centered on the output the compositor chooses for a surface without an assigned output (typically its focused output). To learn the cursor position on compositors that report it only after pointer motion, the system MAY request a zero-distance cursor move through the compositor's own IPC; it MUST NOT move the cursor visibly or deliver key or text input.

#### Scenario: Cursor in middle of screen
- **WHEN** the pointer is at the center of the output and the trigger fires
- **THEN** the popup's top-left corner appears next to the pointer

#### Scenario: Cursor near bottom-right edge
- **WHEN** the pointer is 20 logical pixels from the bottom-right corner of the output
- **THEN** the popup is shifted up and left so that it is entirely on-screen

#### Scenario: Second monitor with fractional scale
- **WHEN** the pointer is on a second output scaled at 1.5 positioned to the right of the first
- **THEN** the popup appears on the second output next to the pointer

#### Scenario: Sway without pointer motion
- **WHEN** the trigger fires on Sway and the pointer has not moved since the popup was requested
- **THEN** the popup appears next to the pointer rather than centered

#### Scenario: Cursor position unknown
- **WHEN** the cursor position cannot be determined within 50 ms
- **THEN** the popup is centered on the output chosen by the compositor

### Requirement: Placement on compositors without layer-shell
On compositors that do not advertise wlr-layer-shell (such as GNOME), the popup SHALL open as a normal application window whose position is chosen by the compositor.

#### Scenario: GNOME
- **WHEN** the trigger fires on GNOME
- **THEN** the popup opens as a normal window centered by the compositor
- **AND** all popup functions other than cursor placement behave the same as on layer-shell compositors

### Requirement: User-resizable popup
The user SHALL be able to resize the popup by dragging its edges and corners on all supported compositors. The popup MUST enforce a minimum size that keeps both panes and header controls usable. The last size SHALL be persisted and reused for subsequent popups; position MUST NOT be persisted.

#### Scenario: Resize on layer-shell compositor
- **WHEN** the user drags the popup's bottom-right corner on KDE Plasma
- **THEN** the popup grows with the drag while its top-left corner stays fixed

#### Scenario: Minimum size
- **WHEN** the user drags an edge inward past the minimum size
- **THEN** the popup stops shrinking at the minimum size

#### Scenario: Size remembered
- **WHEN** the user resizes the popup to 900x400, closes it, and triggers again
- **THEN** the new popup is 900x400 and positioned at the current cursor

### Requirement: Fast appearance
When the trigger reaches a running daemon, the popup SHALL be visible within 100 ms. Selection capture MUST NOT hold back the popup: if capture has not finished when the popup is shown, the source pane SHALL show a placeholder and be filled as soon as capture completes. The translation pane SHALL show a loading indicator until the translation arrives.

#### Scenario: Warm trigger
- **WHEN** the daemon is running and the user fires the trigger
- **THEN** the popup is visible within 100 ms
- **AND** the source pane shows the captured text as soon as capture completes
- **AND** the translation pane shows a loading indicator until the result arrives

#### Scenario: Slow selection owner
- **WHEN** the application owning PRIMARY takes 140 ms to deliver the selection
- **THEN** the popup is already visible with a placeholder in the source pane
- **AND** the placeholder is replaced by the captured text once it arrives

### Requirement: Copy and close
The popup SHALL take keyboard focus when shown. Pressing Ctrl+C SHALL copy the text selected inside either pane if there is a selection, otherwise the full translation, to CLIPBOARD and then close the popup. Clicking the Copy button SHALL copy the full translation and close the popup. Copied text MUST remain available on CLIPBOARD after the popup closes for as long as the daemon runs and no other application takes the clipboard.

#### Scenario: Ctrl+C without selection
- **WHEN** the translation is "Hello world", no text is selected in the popup, and the user presses Ctrl+C
- **THEN** CLIPBOARD contains "Hello world" and the popup closes

#### Scenario: Ctrl+C with selection
- **WHEN** the user selects "world" in the translation pane and presses Ctrl+C
- **THEN** CLIPBOARD contains "world" and the popup closes

#### Scenario: Paste after close
- **WHEN** the user copies via the Copy button and then pastes into another application
- **THEN** the translation is pasted

#### Scenario: Ctrl+C while translation pending
- **WHEN** the translation has not arrived, nothing is selected, and the user presses Ctrl+C
- **THEN** nothing is copied and the popup stays open

### Requirement: Dismissal and toggle
Pressing Esc SHALL close the popup without copying. Clicking outside the popup or the popup losing keyboard focus SHALL close it. Firing the trigger while the popup is visible SHALL hide it. Closing MUST cancel any in-flight translation.

#### Scenario: Esc
- **WHEN** the popup is open and the user presses Esc
- **THEN** the popup closes and CLIPBOARD is unchanged

#### Scenario: Click outside
- **WHEN** the user clicks on another window
- **THEN** the popup closes

#### Scenario: Toggle
- **WHEN** the popup is visible and the user fires the trigger again
- **THEN** the popup hides

### Requirement: Source application is never modified
The system MUST NOT type into, paste into, or otherwise alter the source application or its text.

#### Scenario: After copy
- **WHEN** the user copies the translation and the popup closes
- **THEN** the text in the source application is unchanged and no input is sent to it

### Requirement: Theme follows system preference
The popup SHALL follow the system light/dark preference as reported by the XDG Settings portal color-scheme, and MUST update if the preference changes while the daemon runs. Without a portal value, it SHALL use the light theme.

#### Scenario: Dark mode
- **WHEN** the system color-scheme is "prefer-dark"
- **THEN** the popup renders with the dark theme

#### Scenario: Runtime change
- **WHEN** the user switches the system to light mode while the daemon runs
- **THEN** the next popup renders with the light theme

### Requirement: Keyboard navigation
All popup controls SHALL be reachable with Tab and Shift+Tab and operable with the keyboard alone.

#### Scenario: Change target language by keyboard
- **WHEN** the user presses Tab until the target-language selector is focused and chooses a language with arrow keys and Enter
- **THEN** the target language changes and a new translation starts
