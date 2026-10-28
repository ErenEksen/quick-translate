//! Builds the active translator from the config and the stored key. Runs at startup and after
//! settings changes, never on the trigger path.

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

use super::core::Backend;
use super::persist::Change;

pub fn secret_id(kind: BackendKind) -> BackendId {
    match kind {
        BackendKind::Deepl => BackendId::Deepl,
        BackendKind::Openai => BackendId::Openai,
    }
}

/// The translator for `config.backend`. A missing DeepL key yields a backend the popup shows
/// as not configured.
pub async fn build(
    config: &Config,
    secrets: &SecretStore,
    http: &HttpClient,
    persist: &mpsc::UnboundedSender<Change>,
) -> Backend {
    let key = match secrets.get(secret_id(config.backend)).await {
        Ok(key) => key.unwrap_or_default(),
        Err(err) => {
            tracing::warn!("cannot read the API key: {err}");
            String::new()
        }
    };
    let timeout = Duration::from_secs(config.request_timeout_secs.max(1));
    match config.backend {
        BackendKind::Deepl => {
            if key.trim().is_empty() {
                return not_configured("DeepL", true);
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
                    label: "DeepL".to_owned(),
                    configured: true,
                    reports_detected: true,
                },
            }
        }
        BackendKind::Openai => {
            let label = format!("AI: {}", config.openai.model);
            if config.openai.base_url.trim().is_empty() || config.openai.model.trim().is_empty() {
                return not_configured(&label, false);
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
                    label,
                    configured: true,
                    reports_detected: false,
                },
            }
        }
    }
}

fn not_configured(label: &str, reports_detected: bool) -> Backend {
    Backend {
        translator: Arc::new(NotConfigured),
        info: BackendInfo {
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

    #[tokio::test]
    async fn deepl_without_key_is_not_configured() {
        let secrets = SecretStore::session_only();
        let http = build_client(Duration::from_secs(5)).expect("client");
        let (tx, _rx) = mpsc::unbounded_channel();
        let backend = build(&Config::default(), &secrets, &http, &tx).await;
        assert!(!backend.info.configured);
        assert_eq!(backend.info.label, "DeepL");

        secrets.set_for_session(BackendId::Deepl, "abc:fx");
        let backend = build(&Config::default(), &secrets, &http, &tx).await;
        assert!(backend.info.configured);
        assert!(backend.info.reports_detected);
    }

    #[tokio::test]
    async fn openai_key_is_optional() {
        let secrets = SecretStore::session_only();
        let http = build_client(Duration::from_secs(5)).expect("client");
        let (tx, _rx) = mpsc::unbounded_channel();
        let config = Config {
            backend: BackendKind::Openai,
            ..Config::default()
        };
        let backend = build(&config, &secrets, &http, &tx).await;
        assert!(backend.info.configured);
        assert!(!backend.info.reports_detected);
        assert_eq!(backend.info.label, "AI: gpt-4o-mini");
    }
}
