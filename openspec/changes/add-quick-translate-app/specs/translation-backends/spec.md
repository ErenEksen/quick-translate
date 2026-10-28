# Spec Delta

## Purpose

Translates captured text through exactly two user-configured services, DeepL (default) and an OpenAI-compatible chat completions endpoint, with credentials kept in the desktop keyring and clear error reporting.

## ADDED Requirements

### Requirement: Backend selection
The system SHALL offer exactly two translation backends: DeepL and OpenAI-compatible. DeepL MUST be the default. The user SHALL select the active backend in the settings window, and the change MUST take effect on the next translation without restarting. The system MUST NOT include other online backends or offline translation models.

#### Scenario: Default backend
- **WHEN** the app runs for the first time
- **THEN** DeepL is the active backend

#### Scenario: Switch backend
- **WHEN** the user selects OpenAI-compatible in settings and triggers a translation
- **THEN** the translation is requested from the configured OpenAI-compatible endpoint and the popup's backend indicator shows it

#### Scenario: Backend not configured
- **WHEN** the active backend has no credentials or endpoint configured and the user triggers a translation
- **THEN** the translation pane shows a message explaining that the backend is not configured, with an action that opens the settings window

### Requirement: Credential storage in Secret Service
API keys SHALL be stored only in the desktop keyring through the Secret Service API, under the app ID `io.github.quicktranslate.QuickTranslate`. Keys MUST NOT be written to the configuration file, logs, or any other file. If no Secret Service provider is available, the system MUST tell the user and SHALL offer session-only use, keeping the key in memory until the daemon exits.

#### Scenario: Key saved
- **WHEN** the user saves a DeepL API key in settings
- **THEN** the key is stored in the Secret Service and `config.toml` contains no key material

#### Scenario: No keyring available
- **WHEN** no Secret Service provider is running and the user saves a key
- **THEN** the settings window explains that the key cannot be stored persistently and offers to use it for this session only
- **AND** after the daemon restarts, the key must be entered again

### Requirement: DeepL key setup
The DeepL settings SHALL accept a pasted API key and provide a "Get API key" button that opens DeepL's API key page in the default browser. On save, the system SHALL validate the key by calling the DeepL usage endpoint and report success or the failure reason.

#### Scenario: Get API key
- **WHEN** the user clicks "Get API key"
- **THEN** the DeepL API key page opens in the default browser

#### Scenario: Valid key
- **WHEN** the user saves a valid key
- **THEN** settings shows the key as valid and displays current character usage

#### Scenario: Invalid key
- **WHEN** the user saves a key that DeepL rejects with HTTP 403
- **THEN** settings shows "Invalid API key" and the key is not marked as valid

### Requirement: DeepL endpoint selection
The system SHALL send requests for keys ending in `:fx` to `api-free.deepl.com` and other keys to `api.deepl.com`. If the chosen endpoint returns HTTP 403, the system SHALL retry once on the other endpoint and, on success, remember that endpoint for the key.

#### Scenario: Free key
- **WHEN** the key ends in `:fx`
- **THEN** requests go to `api-free.deepl.com`

#### Scenario: Endpoint fallback
- **WHEN** a key without the `:fx` suffix is rejected with 403 by `api.deepl.com` but accepted by `api-free.deepl.com`
- **THEN** the translation succeeds and later requests with that key go directly to `api-free.deepl.com`

### Requirement: DeepL translation and quota
DeepL translations SHALL omit the source language when the popup is set to "Auto-detect" and SHALL report the detected source language to the popup. The settings window SHALL show the character count used and the character limit returned by the DeepL usage endpoint.

#### Scenario: Auto-detect
- **WHEN** the source is "Auto-detect" and the text is "Guten Morgen"
- **THEN** DeepL is called without a source language and the popup shows German as the detected language

#### Scenario: Quota display
- **WHEN** the user opens the settings window with a valid DeepL key
- **THEN** it shows the characters used and the character limit, for example "120,000 / 1,000,000"

### Requirement: DeepL error handling
The system SHALL map DeepL failures to user-facing messages in the translation pane: HTTP 403 as invalid key, HTTP 456 as quota exceeded, network failures and timeouts as connection errors. On HTTP 429 the system SHALL retry automatically with exponential backoff up to 3 times before showing a rate-limit message.

#### Scenario: Quota exceeded
- **WHEN** DeepL responds with HTTP 456
- **THEN** the translation pane shows that the DeepL quota is exhausted

#### Scenario: Rate limited
- **WHEN** DeepL responds with HTTP 429 twice and then succeeds
- **THEN** the translation is shown without an error message

#### Scenario: Offline
- **WHEN** the network is unavailable
- **THEN** the translation pane shows a connection error and the popup remains usable

### Requirement: OpenAI-compatible configuration
The OpenAI-compatible settings SHALL accept a base URL (for example `https://api.openai.com/v1` or `http://localhost:11434/v1`), a model name, and an optional API key, which may be empty for local servers. A "Test connection" button SHALL send a short translation request and report success or the error.

#### Scenario: Local server without key
- **WHEN** the user sets base URL `http://localhost:11434/v1`, model `llama3`, no API key, and clicks "Test connection"
- **THEN** the request is sent without an Authorization header and the result is reported

#### Scenario: Hosted service with key
- **WHEN** the user sets an API key for `https://api.openai.com/v1`
- **THEN** requests include the key as a Bearer token

#### Scenario: Wrong model
- **WHEN** the endpoint returns an error for an unknown model during "Test connection"
- **THEN** settings shows the endpoint's error message

### Requirement: Streaming AI translation
The OpenAI-compatible backend SHALL use the chat completions API with streaming enabled and SHALL append received tokens to the translation pane progressively. The request SHALL instruct the model to output only the translation into the target language, preserving line breaks and formatting. This backend SHALL show the source language as "Auto" and MUST NOT display a detected language.

#### Scenario: Progressive output
- **WHEN** the user triggers a translation with the OpenAI-compatible backend
- **THEN** the translation pane fills in incrementally as tokens arrive

#### Scenario: Multi-line text
- **WHEN** the source text has three lines
- **THEN** the translation has three corresponding lines without added commentary

### Requirement: Request cancellation and timeouts
Any in-flight request SHALL be cancelled when the popup closes or the source text, source language, or target language changes. Requests MUST use a 5-second connect timeout and a total timeout that defaults to 30 seconds and is configurable.

#### Scenario: Popup closed during streaming
- **WHEN** the popup is closed while tokens are still streaming
- **THEN** the HTTP request is aborted

#### Scenario: Slow endpoint
- **WHEN** an endpoint does not complete a response within the configured total timeout
- **THEN** the request is aborted and the translation pane shows a timeout error

### Requirement: Transport security and privacy
Text SHALL be sent only to the active backend's endpoint. The system MUST refuse plain `http://` base URLs for hosts other than loopback addresses unless the user explicitly confirms the insecure endpoint in settings. The system MUST NOT send telemetry or analytics.

#### Scenario: Insecure remote endpoint
- **WHEN** the user enters base URL `http://example.com/v1`
- **THEN** settings refuses to save it until the user explicitly confirms using an unencrypted connection

#### Scenario: Loopback over HTTP
- **WHEN** the user enters base URL `http://127.0.0.1:8080/v1`
- **THEN** the setting is accepted without confirmation

#### Scenario: No telemetry
- **WHEN** the app runs and translates text
- **THEN** no network requests are made other than to the active backend's endpoint, the DeepL usage endpoint, and the "Get API key" page opened in the browser
