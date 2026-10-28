//! API key storage in the Secret Service with a session-only in-memory fallback (design D8).
//!
//! Items carry the attributes `{application: io.github.quicktranslate.QuickTranslate,
//! backend: deepl|openai}` and live in the provider's default collection. When no provider is
//! reachable, keys are kept in memory until the process exits and are never written to disk.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use oo7::dbus::{Collection, Service};
use oo7::zbus;

/// Keyring `application` attribute. Equal to `quick_translate_core::APP_ID`.
pub const APPLICATION: &str = "io.github.quicktranslate.QuickTranslate";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Which backend a key belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendId {
    Deepl,
    Openai,
}

impl BackendId {
    /// The `backend` attribute value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deepl => "deepl",
            Self::Openai => "openai",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Deepl => "Quick Translate DeepL API key",
            Self::Openai => "Quick Translate OpenAI-compatible API key",
        }
    }

    fn attributes(self) -> [(&'static str, &'static str); 2] {
        [("application", APPLICATION), ("backend", self.as_str())]
    }
}

/// A keyring operation failed. Never contains secret material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    /// The user dismissed the keyring's unlock or create prompt.
    #[error("The keyring prompt was dismissed")]
    Dismissed,
    #[error("Keyring error: {0}")]
    Service(String),
}

impl From<oo7::dbus::Error> for SecretError {
    fn from(e: oo7::dbus::Error) -> Self {
        match e {
            oo7::dbus::Error::Dismissed => Self::Dismissed,
            other => Self::Service(other.to_string()),
        }
    }
}

/// API key store. Persistent when a Secret Service provider exists, session-only otherwise.
///
/// Keys set with [`SecretStore::set_for_session`] (or while no provider exists) shadow the
/// keyring for the lifetime of this value.
pub struct SecretStore {
    service: Option<Service>,
    session: Mutex<HashMap<BackendId, String>>,
}

impl std::fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretStore")
            .field("persistent", &self.is_persistent())
            .finish_non_exhaustive()
    }
}

impl SecretStore {
    /// Connects to the Secret Service on the session bus, falling back to session-only storage
    /// when there is no bus or no provider.
    pub async fn connect() -> Self {
        let cnx = async {
            zbus::connection::Builder::session()?
                .method_timeout(Duration::from_secs(30))
                .build()
                .await
        };
        match tokio::time::timeout(CONNECT_TIMEOUT, cnx).await {
            Ok(Ok(cnx)) => Self::with_connection(&cnx).await,
            Ok(Err(e)) => {
                tracing::info!(error = %e, "no session bus, API keys are kept for this session only");
                Self::session_only()
            }
            Err(_) => {
                tracing::info!("session bus timed out, API keys are kept for this session only");
                Self::session_only()
            }
        }
    }

    /// Like [`Self::connect`] on the bus at `address` (e.g. a private test bus).
    pub async fn connect_to(address: &str) -> Self {
        let cnx = async {
            zbus::connection::Builder::address(address)?
                .method_timeout(Duration::from_secs(30))
                .build()
                .await
        };
        match tokio::time::timeout(CONNECT_TIMEOUT, cnx).await {
            Ok(Ok(cnx)) => Self::with_connection(&cnx).await,
            _ => Self::session_only(),
        }
    }

    /// Uses an existing D-Bus connection.
    pub async fn with_connection(cnx: &zbus::Connection) -> Self {
        match tokio::time::timeout(CONNECT_TIMEOUT, Service::new_with_connection(cnx)).await {
            Ok(Ok(service)) => {
                tracing::debug!("connected to the Secret Service");
                Self {
                    service: Some(service),
                    session: Mutex::default(),
                }
            }
            Ok(Err(e)) => {
                tracing::info!(error = %e, "no Secret Service provider, API keys are kept for this session only");
                Self::session_only()
            }
            Err(_) => {
                tracing::info!("Secret Service timed out, API keys are kept for this session only");
                Self::session_only()
            }
        }
    }

    /// An in-memory store that forgets everything when dropped.
    pub fn session_only() -> Self {
        Self {
            service: None,
            session: Mutex::default(),
        }
    }

    /// Whether keys survive a daemon restart. Settings must tell the user when this is false.
    pub fn is_persistent(&self) -> bool {
        self.service.is_some()
    }

    /// Returns the stored key, if any.
    pub async fn get(&self, id: BackendId) -> Result<Option<String>, SecretError> {
        if let Some(key) = self.session_get(id) {
            return Ok(Some(key));
        }
        let Some(service) = &self.service else {
            return Ok(None);
        };
        let Some(collection) = service.with_alias(Service::DEFAULT_COLLECTION).await? else {
            return Ok(None);
        };
        let items = collection.search_items(&id.attributes()).await?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        if item.is_locked().await? {
            item.unlock(None).await?;
        }
        let secret = item.secret().await?;
        Ok(Some(
            String::from_utf8_lossy(secret.as_bytes()).into_owned(),
        ))
    }

    /// Stores `key`, replacing any previous one. Persistent when [`Self::is_persistent`].
    pub async fn set(&self, id: BackendId, key: &str) -> Result<(), SecretError> {
        let Some(service) = &self.service else {
            self.set_for_session(id, key);
            return Ok(());
        };
        let collection = unlocked_default(service).await?;
        collection
            .create_item(
                id.label(),
                &id.attributes(),
                oo7::Secret::text(key),
                true,
                None,
            )
            .await?;
        self.session_lock().remove(&id);
        tracing::debug!(backend = id.as_str(), "API key stored in the keyring");
        Ok(())
    }

    /// Keeps `key` in memory only, e.g. after the user declined persistent storage.
    pub fn set_for_session(&self, id: BackendId, key: &str) {
        self.session_lock().insert(id, key.to_owned());
        tracing::debug!(backend = id.as_str(), "API key kept for this session only");
    }

    /// Drops a session-only key so the keyring item (if any) is used again. The keyring is
    /// not touched.
    pub fn clear_session(&self, id: BackendId) {
        self.session_lock().remove(&id);
    }

    /// Removes the key from memory and from the keyring.
    pub async fn delete(&self, id: BackendId) -> Result<(), SecretError> {
        self.session_lock().remove(&id);
        let Some(service) = &self.service else {
            return Ok(());
        };
        let Some(collection) = service.with_alias(Service::DEFAULT_COLLECTION).await? else {
            return Ok(());
        };
        if collection.is_locked().await? {
            collection.unlock(None).await?;
        }
        for item in collection.search_items(&id.attributes()).await? {
            item.delete(None).await?;
        }
        Ok(())
    }

    fn session_get(&self, id: BackendId) -> Option<String> {
        self.session_lock().get(&id).cloned()
    }

    fn session_lock(&self) -> std::sync::MutexGuard<'_, HashMap<BackendId, String>> {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

async fn unlocked_default(service: &Service) -> Result<Collection, SecretError> {
    let collection = service.default_collection().await?;
    if collection.is_locked().await? {
        collection.unlock(None).await?;
    }
    Ok(collection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_only_round_trip() {
        let store = SecretStore::session_only();
        assert!(!store.is_persistent());
        assert_eq!(store.get(BackendId::Deepl).await, Ok(None));
        store.set(BackendId::Deepl, "k:fx").await.unwrap();
        store.set(BackendId::Openai, "sk-1").await.unwrap();
        assert_eq!(store.get(BackendId::Deepl).await, Ok(Some("k:fx".into())));
        store.set(BackendId::Deepl, "k2:fx").await.unwrap();
        assert_eq!(store.get(BackendId::Deepl).await, Ok(Some("k2:fx".into())));
        store.delete(BackendId::Deepl).await.unwrap();
        assert_eq!(store.get(BackendId::Deepl).await, Ok(None));
        assert_eq!(store.get(BackendId::Openai).await, Ok(Some("sk-1".into())));
        store.clear_session(BackendId::Openai);
        assert_eq!(store.get(BackendId::Openai).await, Ok(None));
        drop(store);
        assert_eq!(
            SecretStore::session_only().get(BackendId::Openai).await,
            Ok(None)
        );
    }

    #[test]
    fn attributes() {
        assert_eq!(
            BackendId::Openai.attributes(),
            [
                ("application", "io.github.quicktranslate.QuickTranslate"),
                ("backend", "openai")
            ]
        );
        assert_eq!(BackendId::Deepl.as_str(), "deepl");
    }

    #[test]
    fn debug_has_no_secrets() {
        let store = SecretStore::session_only();
        store.set_for_session(BackendId::Deepl, "very-secret");
        assert!(!format!("{store:?}").contains("very-secret"));
    }
}
