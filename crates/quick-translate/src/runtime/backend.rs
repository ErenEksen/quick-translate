//! Builds the active translator from the config and the stored keys. Runs at startup and after
//! settings changes, never on the trigger path. When DeepL and an AI endpoint are both set up,
//! the other one is built too, for the popup's DeepL/AI switch.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use quick_translate_backends::{
    ChunkStream, DeeplBackend, DeeplHost, DeeplOptions, Error, HttpClient, OpenAiBackend,
    OpenAiOptions, Request, Translator,
};
use quick_translate_core::{BackendKind, Config};
use quick_translate_secrets::{BackendId, SecretStore};
use quick_translate_ui::popup::BackendInfo;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::core::{Backend, Backends};
use super::persist::Change;

pub fn secret_id(kind: BackendKind) -> BackendId {
    match kind {
        BackendKind::Deepl => BackendId::Deepl,
        BackendKind::Openai => BackendId::Openai,
    }
}

fn other(kind: BackendKind) -> BackendKind {
    match kind {
        BackendKind::Deepl => BackendKind::Openai,
        BackendKind::Openai => BackendKind::Deepl,
    }
}

/// The stored key for `kind`; `Err` when the keyring could not be read.
async fn read_key(secrets: &SecretStore, kind: BackendKind) -> Result<String, ()> {
    match secrets.get(secret_id(kind)).await {
        Ok(key) => Ok(key.unwrap_or_default()),
        Err(err) => {
            tracing::warn!(backend = ?kind, "cannot read the API key: {err}");
            Err(())
        }
    }
}

/// Whether `kind` is set up well enough to be offered by the popup's switch: DeepL needs a
/// key, an AI endpoint a key or a non-default base URL (see `OpenAiConfig::is_set_up`).
fn is_set_up(config: &Config, kind: BackendKind, key: &str) -> bool {
    let has_key = !key.trim().is_empty();
    match kind {
        BackendKind::Deepl => has_key,
        BackendKind::Openai => config.openai.is_set_up(has_key),
    }
}

/// The translator for `config.backend`, plus the other backend when both are set up. A
/// missing DeepL key yields a backend the popup shows as not configured.
pub async fn build(
    config: &Config,
    secrets: &SecretStore,
    http: &HttpClient,
    persist: &mpsc::UnboundedSender<Change>,
) -> Backends {
    let active_kind = config.backend;
    let active_key = read_key(secrets, active_kind).await;
    let active = build_one(
        config,
        active_kind,
        active_key.clone().unwrap_or_default(),
        http,
        persist,
    );
    // The second key is read only when the first read worked: a locked keyring whose unlock
    // prompt was dismissed must not prompt twice.
    let alternate = match active_key {
        Ok(key) if active.info.configured && is_set_up(config, active_kind, &key) => {
            let other_kind = other(active_kind);
            match read_key(secrets, other_kind).await {
                Ok(other_key) if is_set_up(config, other_kind, &other_key) => {
                    let backend = build_one(config, other_kind, other_key, http, persist);
                    backend.info.configured.then_some(backend)
                }
                _ => None,
            }
        }
        _ => None,
    };
    Backends { active, alternate }
}

/// The translator for `kind` with `key` (possibly empty).
fn build_one(
    config: &Config,
    kind: BackendKind,
    key: String,
    http: &HttpClient,
    persist: &mpsc::UnboundedSender<Change>,
) -> Backend {
    let timeout = Duration::from_secs(config.request_timeout_secs.max(1));
    match kind {
        BackendKind::Deepl => {
            if key.trim().is_empty() {
                return not_configured(BackendKind::Deepl, "DeepL", true);
            }
            let options = DeeplOptions {
                cached_host: config
                    .deepl
                    .cached_host
                    .as_deref()
                    .and_then(DeeplHost::from_hostname),
                timeout,
                ..DeeplOptions::new(key)
            };
            let persist = persist.clone();
            let backend = DeeplBackend::new(http.clone(), options).on_host_change(move |host| {
                let _ = persist.send(Change::DeeplHost(host));
            });
            Backend {
                translator: Arc::new(backend),
                info: BackendInfo {
                    kind: BackendKind::Deepl,
                    label: "DeepL".to_owned(),
                    configured: true,
                    reports_detected: true,
                },
            }
        }
        BackendKind::Openai => {
            let label = format!("AI: {}", config.openai.model);
            if config.openai.base_url.trim().is_empty() || config.openai.model.trim().is_empty() {
                return not_configured(BackendKind::Openai, &label, false);
            }
            let options = OpenAiOptions {
                api_key: key,
                allow_insecure: config.openai.allow_insecure,
                timeout,
                ..OpenAiOptions::new(&config.openai.base_url, &config.openai.model)
            };
            Backend {
                translator: Arc::new(OpenAiBackend::new(http.clone(), options)),
                info: BackendInfo {
                    kind: BackendKind::Openai,
                    label,
                    configured: true,
                    reports_detected: false,
                },
            }
        }
    }
}

fn not_configured(kind: BackendKind, label: &str, reports_detected: bool) -> Backend {
    Backend {
        translator: Arc::new(NotConfigured),
        info: BackendInfo {
            kind,
            label: label.to_owned(),
            configured: false,
            reports_detected,
        },
    }
}

/// Stands in for a backend without credentials; the popup never calls it.
struct NotConfigured;

impl Translator for NotConfigured {
    fn translate(
        &self,
        _req: Request,
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = quick_translate_backends::Result<ChunkStream>> + Send + '_>>
    {
        Box::pin(async { Err(Error::NotConfigured) })
    }
}

#[cfg(test)]
mod tests {
    use quick_translate_backends::build_client;

    use super::*;

    fn deps() -> (SecretStore, HttpClient, mpsc::UnboundedSender<Change>) {
        let (tx, _rx) = mpsc::unbounded_channel();
        let http = build_client(Duration::from_secs(5)).expect("client");
        (SecretStore::session_only(), http, tx)
    }

    fn openai(base_url: &str) -> Config {
        let mut config = Config {
            backend: BackendKind::Openai,
            ..Config::default()
        };
        config.openai.base_url = base_url.to_owned();
        config
    }

    fn kinds(backends: &Backends) -> (BackendKind, Option<BackendKind>) {
        (
            backends.active.info.kind,
            backends.alternate.as_ref().map(|b| b.info.kind),
        )
    }

    #[tokio::test]
    async fn deepl_without_key_is_not_configured() {
        let (secrets, http, tx) = deps();
        let backends = build(&Config::default(), &secrets, &http, &tx).await;
        assert!(!backends.active.info.configured);
        assert_eq!(backends.active.info.label, "DeepL");
        assert_eq!(backends.active.info.kind, BackendKind::Deepl);
        assert!(backends.alternate.is_none());

        secrets.set_for_session(BackendId::Deepl, "abc:fx");
        let backends = build(&Config::default(), &secrets, &http, &tx).await;
        assert!(backends.active.info.configured);
        assert!(backends.active.info.reports_detected);
        // The untouched default AI endpoint without a key is not set up: no switch.
        assert!(backends.alternate.is_none());
    }

    #[tokio::test]
    async fn openai_key_is_optional() {
        let (secrets, http, tx) = deps();
        let config = Config {
            backend: BackendKind::Openai,
            ..Config::default()
        };
        let backends = build(&config, &secrets, &http, &tx).await;
        assert!(backends.active.info.configured);
        assert!(!backends.active.info.reports_detected);
        assert_eq!(backends.active.info.label, "AI: gpt-4o-mini");
        assert_eq!(backends.active.info.kind, BackendKind::Openai);
        assert!(backends.alternate.is_none());
    }

    #[tokio::test]
    async fn both_keys_offer_the_other_backend() {
        let (secrets, http, tx) = deps();
        secrets.set_for_session(BackendId::Deepl, "abc:fx");
        secrets.set_for_session(BackendId::Openai, "sk-test");
        let backends = build(&Config::default(), &secrets, &http, &tx).await;
        assert_eq!(
            kinds(&backends),
            (BackendKind::Deepl, Some(BackendKind::Openai))
        );
        let alternate = backends.alternate.expect("alternate");
        assert!(alternate.info.configured);
        assert_eq!(alternate.info.label, "AI: gpt-4o-mini");

        let config = Config {
            backend: BackendKind::Openai,
            ..Config::default()
        };
        let backends = build(&config, &secrets, &http, &tx).await;
        assert_eq!(
            kinds(&backends),
            (BackendKind::Openai, Some(BackendKind::Deepl))
        );
        assert!(backends.alternate.expect("alternate").info.reports_detected);
    }

    #[tokio::test]
    async fn local_ai_server_without_key_counts_as_set_up() {
        let (secrets, http, tx) = deps();
        secrets.set_for_session(BackendId::Deepl, "abc:fx");
        let mut config = openai("http://localhost:11434/v1");
        let backends = build(&config, &secrets, &http, &tx).await;
        assert_eq!(
            kinds(&backends),
            (BackendKind::Openai, Some(BackendKind::Deepl))
        );
        config.backend = BackendKind::Deepl;
        let backends = build(&config, &secrets, &http, &tx).await;
        assert_eq!(
            kinds(&backends),
            (BackendKind::Deepl, Some(BackendKind::Openai))
        );
    }

    #[tokio::test]
    async fn no_switch_without_deepl_key_or_with_broken_ai_endpoint() {
        let (secrets, http, tx) = deps();
        secrets.set_for_session(BackendId::Openai, "sk-test");
        // AI active, DeepL has no key.
        let backends = build(&openai("https://api.openai.com/v1"), &secrets, &http, &tx).await;
        assert_eq!(kinds(&backends), (BackendKind::Openai, None));
        // DeepL active without a key: the popup offers settings, not a switch.
        let backends = build(&Config::default(), &secrets, &http, &tx).await;
        assert_eq!(kinds(&backends), (BackendKind::Deepl, None));
        assert!(!backends.active.info.configured);

        secrets.set_for_session(BackendId::Deepl, "abc:fx");
        let mut config = Config::default();
        config.openai.model = "  ".into();
        let backends = build(&config, &secrets, &http, &tx).await;
        assert_eq!(kinds(&backends), (BackendKind::Deepl, None));
        // The active AI endpoint without a model is not configured, so no switch either.
        config.backend = BackendKind::Openai;
        let backends = build(&config, &secrets, &http, &tx).await;
        assert_eq!(kinds(&backends), (BackendKind::Openai, None));
        assert!(!backends.active.info.configured);
    }

    #[tokio::test]
    async fn whitespace_keys_count_as_missing() {
        let (secrets, http, tx) = deps();
        secrets.set_for_session(BackendId::Deepl, "abc:fx");
        secrets.set_for_session(BackendId::Openai, "   ");
        let backends = build(&Config::default(), &secrets, &http, &tx).await;
        assert_eq!(kinds(&backends), (BackendKind::Deepl, None));
        secrets.set_for_session(BackendId::Deepl, " ");
        secrets.set_for_session(BackendId::Openai, "sk-test");
        let backends = build(&openai("https://api.openai.com/v1"), &secrets, &http, &tx).await;
        assert_eq!(kinds(&backends), (BackendKind::Openai, None));
    }
}
