//! Writes popup choices and the working DeepL host to `config.toml`, off the UI thread and
//! debounced. Each write re-reads the file first, so settings saved by the settings window are
//! never overwritten, and a malformed file is left untouched (spec daemon-lifecycle).

use std::path::PathBuf;
use std::time::Duration;

use quick_translate_backends::DeeplHost;
use quick_translate_core::{Config, ConfigStore, LoadOutcome};
use tokio::sync::mpsc;

use super::core::Persist;

/// Quiet period before pending changes are written.
const DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Popup(Persist),
    DeeplHost(DeeplHost),
}

/// Starts the writer task. Dropping every sender flushes pending changes and stops it.
pub fn spawn(path: PathBuf) -> mpsc::UnboundedSender<Change> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Change>();
    tokio::spawn(async move {
        while let Some(first) = rx.recv().await {
            let mut changes = vec![first];
            while let Ok(Some(change)) = tokio::time::timeout(DEBOUNCE, rx.recv()).await {
                changes.push(change);
            }
            let path = path.clone();
            let written = tokio::task::spawn_blocking(move || write(&path, &changes)).await;
            if let Err(err) = written {
                tracing::warn!("config writer failed: {err}");
            }
        }
    });
    tx
}

fn write(path: &std::path::Path, changes: &[Change]) {
    let mut store = ConfigStore::new(path);
    let mut config = match store.load() {
        LoadOutcome::Loaded(config) | LoadOutcome::Missing(config) => config,
        LoadOutcome::Malformed { .. } => {
            tracing::info!(
                path = %path.display(),
                "config.toml is malformed; not saving popup changes until settings are saved"
            );
            return;
        }
    };
    for change in changes {
        apply(&mut config, change);
    }
    match store.save(&config) {
        Ok(()) => tracing::debug!(changes = changes.len(), "config saved"),
        Err(err) => tracing::warn!("cannot save the config: {err}"),
    }
}

fn apply(config: &mut Config, change: &Change) {
    match change {
        Change::Popup(Persist::Target(code)) => config.target_lang = Some(code.clone()),
        Change::Popup(Persist::Splitter(ratio)) => config.popup.splitter = *ratio,
        Change::Popup(Persist::Size(size)) => {
            config.popup.width = size.width.round();
            config.popup.height = size.height.round();
        }
        Change::DeeplHost(host) => config.deepl.cached_host = Some(host.hostname().to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use iced::Size;

    use super::*;

    #[test]
    fn changes_merge_into_the_file_and_keep_unknown_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "backend = \"openai\"\nfuture-key = 1\n").expect("write");
        write(
            &path,
            &[
                Change::Popup(Persist::Target("DE".into())),
                Change::Popup(Persist::Size(Size::new(900.4, 399.6))),
                Change::DeeplHost(DeeplHost::Pro),
            ],
        );
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("future-key = 1"), "{text}");
        let config = ConfigStore::new(&path).load().config().clone();
        assert_eq!(config.backend, quick_translate_core::BackendKind::Openai);
        assert_eq!(config.target_lang.as_deref(), Some("DE"));
        assert_eq!((config.popup.width, config.popup.height), (900.0, 400.0));
        assert_eq!(config.deepl.cached_host.as_deref(), Some("api.deepl.com"));
    }

    #[test]
    fn malformed_file_is_left_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "backend = [").expect("write");
        write(&path, &[Change::Popup(Persist::Splitter(0.3))]);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "backend = [");
    }

    #[tokio::test]
    async fn writer_debounces_and_flushes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sub").join("config.toml");
        let tx = spawn(path.clone());
        tx.send(Change::Popup(Persist::Splitter(0.25)))
            .expect("send");
        tx.send(Change::Popup(Persist::Splitter(0.35)))
            .expect("send");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let config = ConfigStore::new(&path).load().config().clone();
            if (config.popup.splitter - 0.35).abs() < f32::EPSILON {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "not written");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
