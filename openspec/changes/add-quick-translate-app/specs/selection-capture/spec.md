# Spec Delta

## Purpose

Obtains the text the user wants translated when the trigger fires, preferring the current PRIMARY selection and degrading gracefully to the clipboard or an empty editable pane, without injecting input or altering the user's selections.

## ADDED Requirements

### Requirement: Background PRIMARY read via data-control
When the trigger fires and the compositor advertises ext-data-control-v1, the system SHALL read the current PRIMARY selection through it without any user prompt. If ext-data-control-v1 is not advertised but wlr-data-control version 2 or later is, the system SHALL use wlr-data-control instead. Protocol availability MUST be detected at runtime from the compositor's advertised globals.

#### Scenario: ext-data-control available
- **WHEN** the user selects "Merhaba dünya" in any application and fires the trigger on a compositor advertising ext-data-control-v1
- **THEN** the popup's source pane contains "Merhaba dünya"
- **AND** no permission dialog is shown

#### Scenario: Only wlr-data-control available
- **WHEN** the compositor advertises wlr-data-control version 2 but not ext-data-control-v1, and the user fires the trigger with text selected
- **THEN** the selected text is read through wlr-data-control and appears in the source pane

#### Scenario: wlr-data-control too old for PRIMARY
- **WHEN** the compositor advertises only wlr-data-control version 1
- **THEN** the system treats background PRIMARY reading as unavailable and proceeds to the focus-time read

### Requirement: Focus-time PRIMARY read fallback
When no usable data-control protocol is available (for example on GNOME/Mutter), the system SHALL show the popup, request keyboard focus for it, and read PRIMARY once focus is received. If the trigger delivered an xdg-activation token, the system MUST use that token when requesting focus.

#### Scenario: GNOME without data-control
- **WHEN** the user selects text and fires the trigger via the GlobalShortcuts portal on a compositor without data-control
- **THEN** the popup is activated using the token from the portal activation
- **AND** after the popup receives keyboard focus, the selected text appears in the source pane

#### Scenario: Focus not granted
- **WHEN** the compositor does not grant keyboard focus to the popup within the capture timeout
- **THEN** the system falls through to the clipboard/empty-pane fallback

### Requirement: Clipboard and empty-pane fallback
If PRIMARY is empty, unavailable, contains no accepted text type, or could not be read, the system SHALL prefill the source pane with the CLIPBOARD's text content if it has any; otherwise it SHALL open the popup with an empty, editable source pane that has keyboard focus.

#### Scenario: Nothing selected, clipboard has text
- **WHEN** PRIMARY is empty and CLIPBOARD contains "Good morning"
- **THEN** the source pane contains "Good morning" and translation starts

#### Scenario: Nothing selected, clipboard empty
- **WHEN** PRIMARY and CLIPBOARD contain no text
- **THEN** the popup opens with an empty source pane focused for typing
- **AND** no translation request is sent until the user enters text

#### Scenario: Clipboard holds only an image
- **WHEN** PRIMARY is empty and CLIPBOARD offers only image/png
- **THEN** the popup opens with an empty, focused source pane

### Requirement: Accepted content types
The system SHALL accept only text offers, in this order of preference: `text/plain;charset=utf-8`, `UTF8_STRING`, `text/plain`. Non-text offers MUST be ignored. Text that is not valid UTF-8 MUST be decoded lossily rather than failing the capture.

#### Scenario: Rich text source
- **WHEN** the selection offers `text/html` and `text/plain;charset=utf-8`
- **THEN** the plain UTF-8 text is used and the HTML is ignored

### Requirement: Text normalization and size limit
Captured text SHALL have leading and trailing whitespace trimmed while preserving internal line breaks. Text longer than 10,000 characters MUST be truncated to 10,000 characters, and the popup MUST show a visible notice that the input was truncated.

#### Scenario: Surrounding whitespace
- **WHEN** the selection is "\n  Hello world  \n"
- **THEN** the source pane contains "Hello world"

#### Scenario: Oversized selection
- **WHEN** the user selects 25,000 characters and fires the trigger
- **THEN** the source pane contains the first 10,000 characters
- **AND** a truncation notice is visible in the popup

#### Scenario: Whitespace-only selection
- **WHEN** PRIMARY contains only spaces and newlines
- **THEN** it is treated as empty and the clipboard/empty-pane fallback applies

### Requirement: Capture time budget
Each capture step SHALL complete or time out within 150 ms, after which the system MUST fall through to the next step. Capture MUST NOT delay showing the popup beyond the popup's visibility budget.

#### Scenario: Source application does not answer
- **WHEN** the application owning PRIMARY does not deliver data within 150 ms
- **THEN** the read is abandoned and the next fallback is used

### Requirement: No input injection or selection modification
The system MUST NOT simulate key presses, inject pointer or keyboard input, or use virtual input devices to obtain the selection. The system MUST NOT change the contents or ownership of the user's PRIMARY selection or CLIPBOARD while capturing.

#### Scenario: Clipboard preserved
- **WHEN** CLIPBOARD contains "A", PRIMARY contains "B", and the user fires the trigger
- **THEN** after capture CLIPBOARD still contains "A" and PRIMARY still contains "B"

#### Scenario: No synthetic copy
- **WHEN** the trigger fires on any supported compositor
- **THEN** no synthetic Ctrl+C or other key event is delivered to any application

### Requirement: Captured text is not persisted
Captured text MUST be held only in memory and MUST NOT be written to disk, the configuration file, or logs.

#### Scenario: Logs after capture
- **WHEN** a selection is captured and translated with debug logging enabled
- **THEN** no log entry and no file on disk contains the captured text
