//! Executes settings [`Effect`]s: config file, keyring, backends and the daemon over D-Bus.
//!
//! Independent of the iced runtime, so tests can drive it on a private bus.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, BoxStream, StreamExt};
use quick_translate_backends::{
    DeeplBackend, DeeplEndpoints, DeeplOptions, HttpClient, OpenAiBackend, OpenAiOptions,
    build_client,
};
use quick_translate_core::{BackendKind, ConfigStore, LoadOutcome};
use quick_translate_ipc::proxy::{QuickTranslateProxy, error_message};
use quick_translate_ipc::settings::backend_name;
use quick_translate_ipc::{DaemonStatus, SecretString, ShortcutInfo};
use quick_translate_secrets::{BackendId, SecretStore};
use zbus::zvariant::{OwnedValue, Value};

use super::{
    DeeplCheck, Effect, Loaded, LoadedMessage, Message, SaveFailure, StoreFailure, Stored,
};

/// Deadline for each call to the daemon or a portal.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

const NO_DAEMON: &str = "The quick-translate daemon is not running";

/// Handles the effects need. Cheap to share behind an `Arc`.
pub struct Runtime {
    config_path: PathBuf,
    connection: Option<zbus::Connection>,
    secrets: Arc<SecretStore>,
    http: HttpClient,
    deepl_endpoints: DeeplEndpoints,
    retry_base_delay: Duration,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("config_path", &self.config_path)
            .field("bus", &self.connection.is_some())
            .field("secrets", &self.secrets)
            .finish_non_exhaustive()
    }
}

impl Runtime {
    /// Connects to the session bus (the daemon and the Secret Service live there).
    pub async fn connect(config_path: PathBuf) -> Self {
        let connection = match tokio::time::timeout(CALL_TIMEOUT, zbus::Connection::session()).await
        {
            Ok(Ok(connection)) => Some(connection),
            Ok(Err(err)) => {
                tracing::warn!(%err, "no session bus");
                None
            }
            Err(_) => {
                tracing::warn!("session bus connection timed out");
                None
            }
        };
        Self::with_connection(config_path, connection).await
    }

    /// Uses `connection` for the daemon and the Secret Service; `None` means no bus.
    pub async fn with_connection(
        config_path: PathBuf,
        connection: Option<zbus::Connection>,
    ) -> Self {
        let secrets = match &connection {
            Some(connection) => SecretStore::with_connection(connection).await,
            None => SecretStore::session_only(),
        };
        Self {
            config_path,
            connection,
            secrets: Arc::new(secrets),
            http: build_client(quick_translate_backends::DEFAULT_TIMEOUT)
                .unwrap_or_else(|_| HttpClient::new()),
            deepl_endpoints: DeeplEndpoints::default(),
            retry_base_delay: Duration::from_millis(500),
        }
    }

    /// Points DeepL calls at other hosts (tests).
    #[must_use]
    pub fn with_deepl_endpoints(mut self, endpoints: DeeplEndpoints) -> Self {
        self.deepl_endpoints = endpoints;
        self.retry_base_delay = Duration::from_millis(1);
        self
    }

    pub fn connection(&self) -> Option<&zbus::Connection> {
        self.connection.as_ref()
    }

    async fn proxy(&self) -> Result<QuickTranslateProxy<'static>, String> {
        let connection = self.connection.as_ref().ok_or("No session bus")?;
        QuickTranslateProxy::builder(connection)
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(|err| err.to_string())
    }

    /// Runs one daemon call with [`CALL_TIMEOUT`], mapping "not running" to [`NO_DAEMON`].
    async fn daemon<T, F>(
        &self,
        call: impl FnOnce(QuickTranslateProxy<'static>) -> F,
    ) -> Result<T, String>
    where
        F: Future<Output = zbus::Result<T>>,
    {
        let proxy = self.proxy().await?;
        match tokio::time::timeout(CALL_TIMEOUT, call(proxy)).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(zbus::Error::MethodError(name, ..)))
                if matches!(
                    name.as_str(),
                    "org.freedesktop.DBus.Error.ServiceUnknown"
                        | "org.freedesktop.DBus.Error.NameHasNoOwner"
                ) =>
            {
                Err(NO_DAEMON.to_owned())
            }
            Ok(Err(err)) => Err(error_message(&err)),
            Err(_) => Err("The daemon did not answer in time".to_owned()),
        }
    }

    /// Subscribes to `ShortcutChanged` and `SettingsRequested` from the daemon and yields them
    /// as messages. Empty without a bus.
    pub async fn signals(&self) -> BoxStream<'static, Message> {
        let Some(connection) = &self.connection else {
            return stream::empty().boxed();
        };
        let subscribed = async {
            let proxy = QuickTranslateProxy::builder(connection)
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
                .await?;
            let changes = proxy.receive_shortcut_changed().await?;
            let raises = proxy.receive_settings_requested().await?;
            Ok::<_, zbus::Error>((changes, raises))
        };
        let (changes, raises) = match subscribed.await {
            Ok(streams) => streams,
            Err(err) => {
                tracing::warn!(%err, "cannot watch daemon signals");
                return stream::empty().boxed();
            }
        };
        let changes = changes.filter_map(|signal| async move {
            let args = signal.args().ok()?;
            Some(Message::TriggerChanged(Some(args.trigger().to_string())))
        });
        let raises = raises.map(|_| Message::RaiseRequested);
        stream::select(changes, raises).boxed()
    }

    /// Reads config, keys and daemon state for [`Message::Loaded`].
    pub async fn load(&self) -> Message {
        let mut store = ConfigStore::new(&self.config_path);
        let (config, config_error) = match store.load() {
            LoadOutcome::Loaded(config) | LoadOutcome::Missing(config) => (config, None),
            LoadOutcome::Malformed { config, error, .. } => (config, Some(error)),
        };
        let mut keyring_error = None;
        let mut key = async |id| match self.secrets.get(id).await {
            Ok(key) => key.map(SecretString::new),
            Err(err) => {
                keyring_error = Some(err.to_string());
                None
            }
        };
        let deepl_key = key(BackendId::Deepl).await;
        let openai_key = key(BackendId::Openai).await;
        let (daemon, shortcut) = self.daemon_state().await;
        Message::Loaded(Box::new(LoadedMessage(Loaded {
            config,
            config_path: self.config_path.clone(),
            config_error,
            keyring_persistent: self.secrets.is_persistent(),
            deepl_key,
            openai_key,
            keyring_error,
            daemon,
            shortcut,
            dark: self.prefers_dark().await,
        })))
    }

    async fn daemon_state(&self) -> (Option<DaemonStatus>, Result<ShortcutInfo, String>) {
        let status = self
            .daemon(|proxy| async move { proxy.status().await })
            .await
            .map(|dict| DaemonStatus::from_dict(&dict));
        let shortcut = match &status {
            Ok(_) => self
                .daemon(|proxy| async move { proxy.shortcut_info().await })
                .await
                .map(|dict| ShortcutInfo::from_dict(&dict)),
            Err(err) => Err(err.clone()),
        };
        (status.ok(), shortcut)
    }

    /// The portal `org.freedesktop.appearance color-scheme` (1 = prefer dark).
    async fn prefers_dark(&self) -> bool {
        let Some(connection) = &self.connection else {
            return false;
        };
        let read = connection.call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.Settings"),
            "ReadOne",
            &("org.freedesktop.appearance", "color-scheme"),
        );
        match tokio::time::timeout(Duration::from_secs(1), read).await {
            Ok(Ok(reply)) => reply
                .body()
                .deserialize::<OwnedValue>()
                .ok()
                .and_then(|value| u32::try_from(&value).ok())
                .is_some_and(|scheme| scheme == 1),
            _ => false,
        }
    }

    /// Executes `effect`. Clipboard and window effects are the runner's and yield `None`.
    pub async fn execute(&self, effect: Effect) -> Option<Message> {
        Some(match effect {
            Effect::SaveConfig(patch) => Message::ConfigSaved(self.save_config(&patch).await),
            Effect::ValidateDeepl { request, key } => Message::DeeplChecked {
                request,
                result: self.validate_deepl(key).await,
            },
            Effect::TestOpenAi {
                request,
                base_url,
                model,
                key,
                allow_insecure,
                target_lang,
            } => {
                let backend = OpenAiBackend::new(
                    self.http.clone(),
                    OpenAiOptions {
                        api_key: key.expose().to_owned(),
                        allow_insecure,
                        ..OpenAiOptions::new(base_url, model)
                    },
                );
                Message::OpenAiTested {
                    request,
                    result: backend.test_connection(&target_lang).await,
                }
            }
            Effect::StoreSecret {
                backend,
                key,
                session_only,
            } => {
                let result = self.store_secret(backend, &key, session_only).await;
                Message::SecretStored {
                    backend,
                    key,
                    result,
                }
            }
            Effect::DeleteSecret(backend) => Message::SecretStored {
                backend,
                key: SecretString::default(),
                result: self.delete_secret(backend).await,
            },
            Effect::OpenUrl(url) => Message::UrlOpened(self.open_url(url).await),
            Effect::RefreshDaemon => {
                let (daemon, shortcut) = self.daemon_state().await;
                Message::DaemonRefreshed { daemon, shortcut }
            }
            Effect::ConfigureShortcut => Message::ShortcutConfigured(
                self.daemon(|proxy| async move { proxy.configure_shortcut("").await })
                    .await,
            ),
            Effect::SetEvdev(enabled) => Message::EvdevSet {
                enabled,
                result: self
                    .daemon(|proxy| async move { proxy.set_evdev_enabled(enabled).await })
                    .await,
            },
            Effect::CopyToClipboard(_) | Effect::Raise => return None,
        })
    }

    async fn save_config(&self, patch: &super::ConfigPatch) -> Result<(), SaveFailure> {
        let mut store = ConfigStore::new(&self.config_path);
        // A malformed file is replaced: the user saved settings.
        let mut config = store.load().config().clone();
        patch.apply(&mut config);
        store
            .save(&config)
            .map_err(|err| SaveFailure::Write(err.to_string()))?;
        self.reload().await.map_err(SaveFailure::Reload)
    }

    async fn reload(&self) -> Result<(), String> {
        self.daemon(|proxy| async move { proxy.reload_config().await })
            .await
    }

    async fn validate_deepl(
        &self,
        key: SecretString,
    ) -> Result<DeeplCheck, quick_translate_backends::Error> {
        let backend = DeeplBackend::new(
            self.http.clone(),
            DeeplOptions {
                endpoints: self.deepl_endpoints.clone(),
                retry_base_delay: self.retry_base_delay,
                ..DeeplOptions::new(key.expose())
            },
        );
        let usage = backend.usage().await?;
        Ok(DeeplCheck {
            usage,
            host: backend.host().hostname(),
        })
    }

    async fn set_session(&self, backend: BackendKind, key: &str) -> Result<(), String> {
        let name = backend_name(backend);
        self.daemon(|proxy| async move { proxy.set_session_secret(name, key).await })
            .await
    }

    async fn store_secret(
        &self,
        backend: BackendKind,
        key: &SecretString,
        session_only: bool,
    ) -> Result<Stored, StoreFailure> {
        if session_only {
            self.set_session(backend, key.expose())
                .await
                .map_err(StoreFailure::Daemon)?;
            self.reload_quietly().await;
            return Ok(Stored::Session);
        }
        self.secrets
            .set(secret_id(backend), key.expose())
            .await
            .map_err(|err| StoreFailure::Keyring(err.to_string()))?;
        // Drop any session key that would shadow the new one, then let the daemon re-read keys.
        let _ = self.set_session(backend, "").await;
        self.reload_quietly().await;
        Ok(Stored::Keyring)
    }

    async fn delete_secret(&self, backend: BackendKind) -> Result<Stored, StoreFailure> {
        self.secrets
            .delete(secret_id(backend))
            .await
            .map_err(|err| StoreFailure::Keyring(err.to_string()))?;
        let _ = self.set_session(backend, "").await;
        self.reload_quietly().await;
        Ok(Stored::Deleted)
    }

    async fn reload_quietly(&self) {
        if let Err(err) = self.reload().await {
            tracing::debug!(%err, "daemon did not reload");
        }
    }

    /// The OpenURI portal, falling back to `xdg-open`.
    async fn open_url(&self, url: &str) -> Result<(), String> {
        if let Some(connection) = &self.connection {
            let options: HashMap<&str, Value<'_>> = HashMap::new();
            let body = ("", url, options);
            let call = connection.call_method(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                Some("org.freedesktop.portal.OpenURI"),
                "OpenURI",
                &body,
            );
            match tokio::time::timeout(CALL_TIMEOUT, call).await {
                Ok(Ok(_)) => return Ok(()),
                Ok(Err(err)) => tracing::debug!(%err, "OpenURI portal failed, trying xdg-open"),
                Err(_) => tracing::debug!("OpenURI portal timed out, trying xdg-open"),
            }
        }
        let status = tokio::process::Command::new("xdg-open")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map_err(|err| format!("Cannot open the browser: {err}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "Cannot open the browser: xdg-open exited with {status}"
            ))
        }
    }
}

fn secret_id(backend: BackendKind) -> BackendId {
    match backend {
        BackendKind::Deepl => BackendId::Deepl,
        BackendKind::Openai => BackendId::Openai,
    }
}
