# Spec Delta

## ADDED Requirements

### Requirement: Recently used languages
Each language selector SHALL list the languages the user picked most recently, newest first and at most three, above the full list and separated from it by a divider. A language counts as picked when the user chooses it in a selector (with the mouse, or with the keyboard once the choice is committed) or when a swap makes it the source or target. "Auto-detect" and the locale default the user never picked SHALL NOT be listed. The lists MUST be remembered across popups and restarts, and unknown or duplicate codes in the stored lists MUST be ignored.

#### Scenario: Recent targets on top
- **WHEN** the user picks German, then French, then Turkish as target and opens the target selector
- **THEN** Turkish, French and German are listed first, in that order, followed by a divider and every target language

#### Scenario: Re-picking moves a language to the front
- **WHEN** the recent targets are Turkish, French and German and the user picks German
- **THEN** the recent targets become German, Turkish and French

#### Scenario: Fourth language
- **WHEN** the recent targets are Turkish, French and German and the user picks Japanese
- **THEN** the recent targets become Japanese, Turkish and French

#### Scenario: Recent sources
- **WHEN** the user picks English as source in one popup and opens the source selector in the next popup
- **THEN** "Auto-detect" is listed first, followed by English, a divider and every source language

#### Scenario: Divider row
- **WHEN** the user clicks the divider row
- **THEN** the menu closes and neither language changes

### Requirement: Switch between DeepL and AI in the popup
When DeepL has an API key and the OpenAI-compatible endpoint is set up (base URL and model filled in, and an API key stored or a base URL other than the default OpenAI endpoint), the backend indicator SHALL be a switch that is off for DeepL and on for the AI model, labelled with the active backend. Flipping it, with the mouse or with Space or Enter when it has keyboard focus, SHALL cancel the request in flight, translate the current text again with the other backend, and remember the choice as the active backend for later popups and restarts. When only one backend is set up, the header SHALL show the backend name without a switch.

#### Scenario: Switch to AI
- **WHEN** both backends are set up, DeepL is active and the user flips the switch in a popup showing a translation
- **THEN** the same text is translated again by the AI model, the source selector shows plain "Auto", and the next popup uses the AI model

#### Scenario: Switch back
- **WHEN** the AI model is active and the user flips the switch
- **THEN** the text is translated again by DeepL and the source selector shows the detected language once DeepL reports it

#### Scenario: Only one backend set up
- **WHEN** a DeepL key is stored and the AI endpoint still has the default base URL and no key
- **THEN** the header shows "DeepL" without a switch

#### Scenario: Results of the previous backend
- **WHEN** the user flips the switch while the previous backend is still streaming a translation
- **THEN** the remaining output of the previous backend is discarded

### Requirement: Header controls stay reachable
The header SHALL keep every control inside the panel at any popup size: when the language controls and the right-hand controls (backend indicator or switch, Copy, settings) do not fit on one line, the right-hand controls SHALL move to a second line, aligned right. The "Translating…" text SHALL only be shown where it fits and MUST NOT move other controls.

#### Scenario: Minimum width
- **WHEN** the popup is 480 px wide and both backends are set up
- **THEN** the language selectors, the swap button, the switch, Copy and the settings button are all visible and usable

#### Scenario: Translation starts
- **WHEN** a translation starts and there is no room for "Translating…" next to the backend indicator
- **THEN** no control moves
