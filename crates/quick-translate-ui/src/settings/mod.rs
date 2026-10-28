//! Settings app core (design D4, `settings` shell): state, messages and update logic.
//!
//! Like the popup core, [`Settings::update`] is pure: it returns [`Effect`]s that the runner
//! executes ([`Runtime::execute`] for I/O, iced tasks for the clipboard and window focus).
//! The settings process writes `config.toml` and the keyring itself and asks the daemon over
//! D-Bus for everything the daemon owns (shortcut, evdev listener, session-only keys).

mod run;
mod runtime;
mod view;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use quick_translate_backends::{EndpointError, Error as BackendError, Usage, check_endpoint};
use quick_translate_core::{BackendKind, Config};
use quick_translate_ipc::{DaemonStatus, SecretString, ShortcutInfo, ShortcutMechanism};

pub use run::{RunOptions, run};
pub use runtime::Runtime;

/// DeepL's API key page, opened by "Get API key".
pub const DEEPL_KEYS_URL: &str = "https://www.deepl.com/your-account/keys";

/// Longest model reply shown after "Test connection".
const TEST_REPLY_CHARS: usize = 80;

/// A change to `config.toml`. The runner re-reads the file and applies only this change, so
/// values the daemon wrote meanwhile (popup size, cached DeepL host) survive.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigPatch {
    Backend(BackendKind),
    /// `None` derives the target from the system locale.
    Target(Option<String>),
    Autostart(bool),
    Evdev(bool),
    DeeplHost(Option<String>),
    OpenAi {
        base_url: String,
        model: String,
        allow_insecure: bool,
    },
}

impl ConfigPatch {
    pub fn apply(&self, config: &mut Config) {
        match self {
            Self::Backend(backend) => config.backend = *backend,
            Self::Target(target) => config.target_lang.clone_from(target),
            Self::Autostart(on) => config.autostart = *on,
            Self::Evdev(on) => config.evdev_enabled = *on,
            Self::DeeplHost(host) => config.deepl.cached_host.clone_from(host),
            Self::OpenAi {
                base_url,
                model,
                allow_insecure,
            } => {
                config.openai.base_url.clone_from(base_url);
                config.openai.model.clone_from(model);
                config.openai.allow_insecure = *allow_insecure;
            }
        }
    }
}

/// Why a config save did not fully apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveFailure {
    /// `config.toml` could not be written.
    Write(String),
    /// Written, but the daemon did not reload it (e.g. not running).
    Reload(String),
}

/// Where a key ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    Keyring,
    /// In daemon memory until it exits.
    Session,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreFailure {
    /// The keyring refused (locked, prompt dismissed, provider error). Session-only use is offered.
    Keyring(String),
    /// The daemon could not take a session-only key.
    Daemon(String),
}

/// A successful DeepL key check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeeplCheck {
    pub usage: Usage,
    /// The host that answered, for config `deepl.cached-host`.
    pub host: &'static str,
}

/// Everything read at startup.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub config_path: PathBuf,
    /// `config.toml` is malformed: defaults are shown and the file is replaced on the next save.
    pub config_error: Option<String>,
    /// A Secret Service provider exists, so keys survive restarts.
    pub keyring_persistent: bool,
    pub deepl_key: Option<SecretString>,
    pub openai_key: Option<SecretString>,
    /// Reading the keyring failed.
    pub keyring_error: Option<String>,
    /// `None` when the daemon is not reachable.
    pub daemon: Option<DaemonStatus>,
    pub shortcut: Result<ShortcutInfo, String>,
    pub dark: bool,
}

/// Input to [`Settings::update`].
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Loaded(Box<LoadedMessage>),
    BackendSelected(BackendKind),
    /// `None` selects the system language.
    TargetSelected(Option<&'static str>),
    AutostartToggled(bool),
    ConfigSaved(Result<(), SaveFailure>),

    DeeplKeyChanged(SecretString),
    DeeplShowKey(bool),
    DeeplSavePressed,
    GetApiKeyPressed,
    UrlOpened(Result<(), String>),
    DeeplChecked {
        request: u64,
        result: Result<DeeplCheck, BackendError>,
    },

    OpenAiUrlChanged(String),
    OpenAiModelChanged(String),
    OpenAiKeyChanged(SecretString),
    OpenAiShowKey(bool),
    OpenAiSavePressed,
    OpenAiTestPressed,
    OpenAiTested {
        request: u64,
        result: Result<String, BackendError>,
    },
    InsecureConfirmed,
    InsecureCancelled,

    SecretStored {
        backend: BackendKind,
        /// The key that was stored (empty after a delete).
        key: SecretString,
        result: Result<Stored, StoreFailure>,
    },
    SessionOnlyConfirmed,
    SessionOnlyCancelled,

    ShortcutLoaded(Result<ShortcutInfo, String>),
    /// `ShortcutChanged` from the daemon.
    TriggerChanged(Option<String>),
    ChangeShortcutPressed,
    ShortcutConfigured(Result<(), String>),
    CopySnippetPressed,
    EvdevToggled(bool),
    EvdevConfirmed,
    EvdevCancelled,
    EvdevSet {
        enabled: bool,
        result: Result<(), String>,
    },

    RefreshPressed,
    DaemonRefreshed {
        daemon: Option<DaemonStatus>,
        shortcut: Result<ShortcutInfo, String>,
    },
    /// `SettingsRequested` from the daemon: raise the window.
    RaiseRequested,
}

/// [`Loaded`] wrapped for [`Message`], which must be `PartialEq`.
#[derive(Debug, Clone)]
pub struct LoadedMessage(pub Loaded);

impl PartialEq for LoadedMessage {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}

/// Side effects requested by [`Settings::update`], executed in order by the runner.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Re-read `config.toml`, apply the patch, write it atomically, then `ReloadConfig()`.
    /// Report with [`Message::ConfigSaved`].
    SaveConfig(ConfigPatch),
    /// `GET /v2/usage` with this key. Report with [`Message::DeeplChecked`].
    ValidateDeepl {
        request: u64,
        key: SecretString,
    },
    /// Send a short translation. Report with [`Message::OpenAiTested`].
    TestOpenAi {
        request: u64,
        base_url: String,
        model: String,
        key: SecretString,
        allow_insecure: bool,
        target_lang: String,
    },
    /// Keyring (then clear the daemon's session key and reload), or `SetSessionSecret` when
    /// `session_only`. Report with [`Message::SecretStored`].
    StoreSecret {
        backend: BackendKind,
        key: SecretString,
        session_only: bool,
    },
    /// Remove the key from the keyring and the daemon. Report with [`Message::SecretStored`].
    DeleteSecret(BackendKind),
    /// OpenURI portal, else `xdg-open`. Report with [`Message::UrlOpened`].
    OpenUrl(&'static str),
    CopyToClipboard(String),
    /// `Status()` and `ShortcutInfo()`. Report with [`Message::DaemonRefreshed`].
    RefreshDaemon,
    /// `ConfigureShortcut()`. Report with [`Message::ShortcutConfigured`].
    ConfigureShortcut,
    /// `SetEvdevEnabled()`. Report with [`Message::EvdevSet`].
    SetEvdev(bool),
    /// Focus the settings window.
    Raise,
}

/// Validation state of the stored DeepL key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeeplStatus {
    /// No key stored.
    Missing,
    /// A key is stored (or was just entered) and has not been checked.
    Unchecked,
    Checking,
    Valid(Usage),
    /// DeepL rejected the key with 403. The key is not stored.
    Invalid,
    /// The key is stored but could not be checked (network, rate limit, ...).
    CheckFailed(String),
    Error(String),
}

/// State of the OpenAI-compatible section's last action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenAiStatus {
    Idle,
    Invalid(String),
    Saved,
    Testing,
    TestOk(String),
    TestFailed(String),
    Error(String),
}

/// The action waiting for the insecure-URL confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsecureAction {
    Save,
    Test,
}

/// Session-only storage offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPrompt {
    pub backend: BackendKind,
    /// Why the key cannot be stored persistently.
    pub reason: SessionReason,
    /// The key to store once confirmed after a keyring failure; `None` re-runs the save.
    key: Option<SecretString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionReason {
    NoProvider,
    KeyringFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortcutState {
    Ready(ShortcutInfo),
    /// The daemon is unreachable or failed to answer.
    Unavailable(String),
}

impl Default for ShortcutState {
    fn default() -> Self {
        Self::Unavailable("Loading…".into())
    }
}

/// One-line feedback in the shortcut section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortcutNotice {
    /// Portal v1: rebind in the system settings.
    Instructions,
    Copied,
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Loading,
    Ready,
}

/// The settings window state.
#[derive(Debug)]
pub struct Settings {
    phase: Phase,
    /// The values as last saved (or loaded).
    config: Config,
    config_path: PathBuf,
    config_error: Option<String>,
    save_error: Option<SaveFailure>,
    keyring_persistent: bool,
    keyring_error: Option<String>,
    /// The user accepted session-only keys in this window.
    session_only: bool,
    session_prompt: Option<SessionPrompt>,
    daemon: Option<DaemonStatus>,
    dark: bool,
    url_error: Option<String>,

    deepl_key: SecretString,
    deepl_saved: Option<SecretString>,
    deepl_show: bool,
    deepl_status: DeeplStatus,
    /// The key being validated for saving (as opposed to the check on open).
    deepl_pending: Option<SecretString>,
    deepl_request: Option<u64>,
    deepl_note: Option<Stored>,

    openai_url: String,
    openai_model: String,
    openai_key: SecretString,
    openai_saved_key: Option<SecretString>,
    openai_show: bool,
    openai_insecure_confirmed: bool,
    insecure_prompt: Option<InsecureAction>,
    openai_status: OpenAiStatus,
    openai_request: Option<u64>,
    openai_note: Option<Stored>,

    shortcut: ShortcutState,
    shortcut_notice: Option<ShortcutNotice>,
    evdev_prompt: bool,
    evdev_busy: bool,
    evdev_error: Option<String>,

    next_request: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self::new()
    }
}

impl Settings {
    /// A window waiting for [`Message::Loaded`].
    pub fn new() -> Self {
        let config = Config::default();
        Self {
            phase: Phase::Loading,
            openai_url: config.openai.base_url.clone(),
            openai_model: config.openai.model.clone(),
            config,
            config_path: PathBuf::new(),
            config_error: None,
            save_error: None,
            keyring_persistent: true,
            keyring_error: None,
            session_only: false,
            session_prompt: None,
            daemon: None,
            dark: false,
            url_error: None,
            deepl_key: SecretString::default(),
            deepl_saved: None,
            deepl_show: false,
            deepl_status: DeeplStatus::Missing,
            deepl_pending: None,
            deepl_request: None,
            deepl_note: None,
            openai_key: SecretString::default(),
            openai_saved_key: None,
            openai_show: false,
            openai_insecure_confirmed: false,
            insecure_prompt: None,
            openai_status: OpenAiStatus::Idle,
            openai_request: None,
            openai_note: None,
            shortcut: ShortcutState::default(),
            shortcut_notice: None,
            evdev_prompt: false,
            evdev_busy: false,
            evdev_error: None,
            next_request: 0,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn is_loading(&self) -> bool {
        self.phase == Phase::Loading
    }

    pub fn dark(&self) -> bool {
        self.dark
    }

    pub fn deepl_status(&self) -> &DeeplStatus {
        &self.deepl_status
    }

    pub fn openai_status(&self) -> &OpenAiStatus {
        &self.openai_status
    }

    pub fn insecure_prompt(&self) -> Option<InsecureAction> {
        self.insecure_prompt
    }

    pub fn session_prompt(&self) -> Option<&SessionPrompt> {
        self.session_prompt.as_ref()
    }

    pub fn shortcut(&self) -> &ShortcutState {
        &self.shortcut
    }

    pub fn shortcut_notice(&self) -> Option<&ShortcutNotice> {
        self.shortcut_notice.as_ref()
    }

    pub fn evdev_prompt(&self) -> bool {
        self.evdev_prompt
    }

    pub fn evdev_error(&self) -> Option<&str> {
        self.evdev_error.as_deref()
    }

    pub fn save_error(&self) -> Option<&SaveFailure> {
        self.save_error.as_ref()
    }

    /// Processes one message and returns the effects to execute, in order.
    #[must_use]
    pub fn update(&mut self, message: Message) -> Vec<Effect> {
        let mut fx = Vec::new();
        if self.phase == Phase::Loading && !matches!(message, Message::Loaded(_)) {
            return fx;
        }
        match message {
            Message::Loaded(loaded) => self.loaded(loaded.0, &mut fx),
            Message::BackendSelected(backend) => {
                self.save(ConfigPatch::Backend(backend), &mut fx);
            }
            Message::TargetSelected(code) => {
                self.save(ConfigPatch::Target(code.map(str::to_owned)), &mut fx);
            }
            Message::AutostartToggled(on) => self.save(ConfigPatch::Autostart(on), &mut fx),
            Message::ConfigSaved(result) => {
                self.save_error = result.err();
                if self.save_error.is_none() {
                    self.config_error = None;
                }
            }

            Message::DeeplKeyChanged(key) => {
                self.deepl_key = key;
                self.deepl_note = None;
            }
            Message::DeeplShowKey(show) => self.deepl_show = show,
            Message::DeeplSavePressed => self.deepl_save(&mut fx),
            Message::GetApiKeyPressed => fx.push(Effect::OpenUrl(DEEPL_KEYS_URL)),
            Message::UrlOpened(result) => self.url_error = result.err(),
            Message::DeeplChecked { request, result } => {
                self.deepl_checked(request, result, &mut fx);
            }

            Message::OpenAiUrlChanged(url) => {
                self.openai_insecure_confirmed = self.config.openai.allow_insecure
                    && url.trim() == self.config.openai.base_url.trim();
                self.openai_url = url;
                self.insecure_prompt = None;
                self.openai_status = OpenAiStatus::Idle;
            }
            Message::OpenAiModelChanged(model) => {
                self.openai_model = model;
                self.openai_status = OpenAiStatus::Idle;
            }
            Message::OpenAiKeyChanged(key) => {
                self.openai_key = key;
                self.openai_note = None;
                self.openai_status = OpenAiStatus::Idle;
            }
            Message::OpenAiShowKey(show) => self.openai_show = show,
            Message::OpenAiSavePressed => self.openai_action(InsecureAction::Save, &mut fx),
            Message::OpenAiTestPressed => self.openai_action(InsecureAction::Test, &mut fx),
            Message::OpenAiTested { request, result } => {
                if self.openai_request == Some(request) {
                    self.openai_request = None;
                    self.openai_status = match result {
                        Ok(reply) => OpenAiStatus::TestOk(excerpt(&reply)),
                        Err(err) => OpenAiStatus::TestFailed(err.to_string()),
                    };
                }
            }
            Message::InsecureConfirmed => {
                if let Some(action) = self.insecure_prompt.take() {
                    self.openai_insecure_confirmed = true;
                    self.openai_action(action, &mut fx);
                }
            }
            Message::InsecureCancelled => self.insecure_prompt = None,

            Message::SecretStored {
                backend,
                key,
                result,
            } => self.secret_stored(backend, key, result),
            Message::SessionOnlyConfirmed => {
                if let Some(prompt) = self.session_prompt.take() {
                    self.session_only = true;
                    match prompt.key {
                        Some(key) => fx.push(Effect::StoreSecret {
                            backend: prompt.backend,
                            key,
                            session_only: true,
                        }),
                        None => match prompt.backend {
                            BackendKind::Deepl => self.deepl_save(&mut fx),
                            BackendKind::Openai => {
                                self.openai_action(InsecureAction::Save, &mut fx);
                            }
                        },
                    }
                }
            }
            Message::SessionOnlyCancelled => self.session_prompt = None,

            Message::ShortcutLoaded(result) => self.set_shortcut(result),
            Message::TriggerChanged(trigger) => {
                if let ShortcutState::Ready(info) = &mut self.shortcut {
                    info.trigger = trigger.filter(|t| !t.is_empty());
                }
            }
            Message::ChangeShortcutPressed => {
                if let ShortcutState::Ready(info) = &self.shortcut {
                    if info.mechanism == ShortcutMechanism::Portal && info.can_configure {
                        self.shortcut_notice = None;
                        fx.push(Effect::ConfigureShortcut);
                    } else {
                        self.shortcut_notice = Some(ShortcutNotice::Instructions);
                    }
                }
            }
            Message::ShortcutConfigured(result) => {
                self.shortcut_notice = result.err().map(ShortcutNotice::Error);
            }
            Message::CopySnippetPressed => {
                if let ShortcutState::Ready(ShortcutInfo {
                    snippet: Some(snippet),
                    ..
                }) = &self.shortcut
                {
                    fx.push(Effect::CopyToClipboard(snippet.text.clone()));
                    self.shortcut_notice = Some(ShortcutNotice::Copied);
                }
            }
            Message::EvdevToggled(true) => {
                if !self.config.evdev_enabled && !self.evdev_busy {
                    self.evdev_prompt = true;
                }
            }
            Message::EvdevToggled(false) => {
                self.evdev_prompt = false;
                if self.config.evdev_enabled && !self.evdev_busy {
                    self.evdev_busy = true;
                    fx.push(Effect::SetEvdev(false));
                }
            }
            Message::EvdevConfirmed => {
                if self.evdev_prompt {
                    self.evdev_prompt = false;
                    self.evdev_busy = true;
                    self.evdev_error = None;
                    fx.push(Effect::SetEvdev(true));
                }
            }
            Message::EvdevCancelled => self.evdev_prompt = false,
            Message::EvdevSet { enabled, result } => {
                self.evdev_busy = false;
                match result {
                    Ok(()) => {
                        self.evdev_error = None;
                        self.save(ConfigPatch::Evdev(enabled), &mut fx);
                        fx.push(Effect::RefreshDaemon);
                    }
                    // Turning it off is saved even when the daemon is unreachable, so it
                    // stays off on the next start.
                    Err(err) if !enabled => {
                        self.evdev_error = Some(err);
                        self.save(ConfigPatch::Evdev(false), &mut fx);
                    }
                    Err(err) => self.evdev_error = Some(err),
                }
            }

            Message::RefreshPressed => fx.push(Effect::RefreshDaemon),
            Message::DaemonRefreshed { daemon, shortcut } => {
                self.daemon = daemon;
                self.set_shortcut(shortcut);
            }
            Message::RaiseRequested => fx.push(Effect::Raise),
        }
        fx
    }

    fn loaded(&mut self, loaded: Loaded, fx: &mut Vec<Effect>) {
        self.phase = Phase::Ready;
        self.openai_url = loaded.config.openai.base_url.clone();
        self.openai_model = loaded.config.openai.model.clone();
        self.openai_insecure_confirmed = loaded.config.openai.allow_insecure;
        self.config = loaded.config;
        self.config_path = loaded.config_path;
        self.config_error = loaded.config_error;
        self.keyring_persistent = loaded.keyring_persistent;
        self.keyring_error = loaded.keyring_error;
        self.daemon = loaded.daemon;
        self.dark = loaded.dark;
        self.set_shortcut(loaded.shortcut);

        self.openai_key = loaded.openai_key.clone().unwrap_or_default();
        self.openai_saved_key = loaded.openai_key;
        self.deepl_key = loaded.deepl_key.clone().unwrap_or_default();
        self.deepl_saved = loaded.deepl_key;
        self.deepl_status = DeeplStatus::Missing;
        if let Some(key) = self.deepl_saved.clone().filter(|k| !k.is_empty()) {
            self.deepl_status = DeeplStatus::Checking;
            let request = self.request_id();
            self.deepl_request = Some(request);
            fx.push(Effect::ValidateDeepl { request, key });
        }
    }

    fn set_shortcut(&mut self, result: Result<ShortcutInfo, String>) {
        self.shortcut = match result {
            Ok(info) => ShortcutState::Ready(info),
            Err(err) => ShortcutState::Unavailable(err),
        };
    }

    fn request_id(&mut self) -> u64 {
        self.next_request += 1;
        self.next_request
    }

    fn save(&mut self, patch: ConfigPatch, fx: &mut Vec<Effect>) {
        patch.apply(&mut self.config);
        fx.push(Effect::SaveConfig(patch));
    }

    /// Keys need confirmation for session-only use when no provider exists.
    fn needs_session_prompt(&mut self, backend: BackendKind) -> bool {
        if self.keyring_persistent || self.session_only {
            return false;
        }
        self.session_prompt = Some(SessionPrompt {
            backend,
            reason: SessionReason::NoProvider,
            key: None,
        });
        true
    }

    fn deepl_save(&mut self, fx: &mut Vec<Effect>) {
        let key = SecretString::new(self.deepl_key.expose().trim());
        self.deepl_note = None;
        if key.is_empty() {
            self.deepl_pending = None;
            self.deepl_request = None;
            self.deepl_status = DeeplStatus::Missing;
            if self.deepl_saved.take().is_some() {
                fx.push(Effect::DeleteSecret(BackendKind::Deepl));
                self.save(ConfigPatch::DeeplHost(None), fx);
            }
            return;
        }
        if self.needs_session_prompt(BackendKind::Deepl) {
            return;
        }
        let request = self.request_id();
        self.deepl_request = Some(request);
        self.deepl_pending = Some(key.clone());
        self.deepl_status = DeeplStatus::Checking;
        fx.push(Effect::ValidateDeepl { request, key });
    }

    fn deepl_checked(
        &mut self,
        request: u64,
        result: Result<DeeplCheck, BackendError>,
        fx: &mut Vec<Effect>,
    ) {
        if self.deepl_request != Some(request) {
            return;
        }
        self.deepl_request = None;
        let pending = self.deepl_pending.take();
        let host = match result {
            Ok(check) => {
                self.deepl_status = DeeplStatus::Valid(check.usage);
                Some(check.host.to_owned())
            }
            Err(BackendError::InvalidKey) => {
                self.deepl_status = DeeplStatus::Invalid;
                return;
            }
            Err(err) => {
                self.deepl_status = DeeplStatus::CheckFailed(err.to_string());
                None
            }
        };
        if let Some(key) = pending {
            fx.push(Effect::StoreSecret {
                backend: BackendKind::Deepl,
                key,
                session_only: !self.keyring_persistent,
            });
            self.save(ConfigPatch::DeeplHost(host), fx);
        }
    }

    /// Validates the endpoint fields; `Ok(allow_insecure)` to proceed.
    fn openai_endpoint(&mut self, action: InsecureAction) -> Option<bool> {
        if self.openai_model.trim().is_empty() {
            self.openai_status = OpenAiStatus::Invalid("Enter a model name".into());
            return None;
        }
        let url = self.openai_url.trim();
        match check_endpoint(url, false) {
            Ok(_) => Some(false),
            Err(EndpointError::NeedsConfirmation) if self.openai_insecure_confirmed => Some(true),
            Err(EndpointError::NeedsConfirmation) => {
                self.insecure_prompt = Some(action);
                self.openai_status = OpenAiStatus::Idle;
                None
            }
            Err(err) => {
                self.openai_status = OpenAiStatus::Invalid(err.to_string());
                None
            }
        }
    }

    fn openai_action(&mut self, action: InsecureAction, fx: &mut Vec<Effect>) {
        let Some(allow_insecure) = self.openai_endpoint(action) else {
            return;
        };
        self.insecure_prompt = None;
        let base_url = self.openai_url.trim().to_owned();
        let model = self.openai_model.trim().to_owned();
        let key = SecretString::new(self.openai_key.expose().trim());
        match action {
            InsecureAction::Test => {
                let request = self.request_id();
                self.openai_request = Some(request);
                self.openai_status = OpenAiStatus::Testing;
                fx.push(Effect::TestOpenAi {
                    request,
                    base_url,
                    model,
                    key,
                    allow_insecure,
                    target_lang: self.config.effective_target_lang().to_owned(),
                });
            }
            InsecureAction::Save => {
                let saved = self.openai_saved_key.clone().unwrap_or_default();
                let key_changed = key != saved;
                if key_changed && !key.is_empty() && self.needs_session_prompt(BackendKind::Openai)
                {
                    return;
                }
                self.openai_request = None;
                self.openai_status = OpenAiStatus::Saved;
                self.openai_note = None;
                self.save(
                    ConfigPatch::OpenAi {
                        base_url,
                        model,
                        allow_insecure,
                    },
                    fx,
                );
                if key_changed {
                    if key.is_empty() {
                        self.openai_saved_key = None;
                        fx.push(Effect::DeleteSecret(BackendKind::Openai));
                    } else {
                        fx.push(Effect::StoreSecret {
                            backend: BackendKind::Openai,
                            key,
                            session_only: !self.keyring_persistent,
                        });
                    }
                }
            }
        }
    }

    fn secret_stored(
        &mut self,
        backend: BackendKind,
        key: SecretString,
        result: Result<Stored, StoreFailure>,
    ) {
        let note = match result {
            Ok(stored) => {
                let saved = (stored != Stored::Deleted && !key.is_empty()).then_some(key);
                match backend {
                    BackendKind::Deepl => self.deepl_saved = saved,
                    BackendKind::Openai => self.openai_saved_key = saved,
                }
                Some(stored)
            }
            Err(StoreFailure::Keyring(reason)) => {
                self.session_prompt = Some(SessionPrompt {
                    backend,
                    reason: SessionReason::KeyringFailed(reason),
                    key: Some(key),
                });
                None
            }
            Err(StoreFailure::Daemon(reason)) => {
                let message = format!("The key was not saved: {reason}");
                match backend {
                    BackendKind::Deepl => self.deepl_status = DeeplStatus::Error(message),
                    BackendKind::Openai => self.openai_status = OpenAiStatus::Error(message),
                }
                None
            }
        };
        match backend {
            BackendKind::Deepl => self.deepl_note = note,
            BackendKind::Openai => self.openai_note = note,
        }
    }
}

/// `120,000 / 1,000,000`.
pub fn format_usage(usage: &Usage) -> String {
    format!(
        "{} / {}",
        group_thousands(usage.character_count),
        group_thousands(usage.character_limit)
    )
}

fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn excerpt(reply: &str) -> String {
    let line = reply.trim().lines().next().unwrap_or_default();
    let mut out: String = line.chars().take(TEST_REPLY_CHARS).collect();
    if line.chars().count() > TEST_REPLY_CHARS {
        out.push('…');
    }
    out
}
