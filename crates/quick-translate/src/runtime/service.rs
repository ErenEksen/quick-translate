//! The daemon's event loop: IPC commands, trigger sources, popup events and settings calls.
//! It runs on the shared Tokio runtime while the popup shell owns the main thread.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use futures::stream::BoxStream;
use quick_translate_backends::HttpClient;
use quick_translate_core::{Capabilities, ConfigStore, LoadOutcome};
use quick_translate_ipc::{DaemonCommand, DaemonStatus, ShortcutInfo, ShortcutMechanism};
use quick_translate_platform::appearance::ColorScheme;
use quick_translate_platform::integration;
use quick_translate_platform::shortcut::evdev::{Confirmed, EvdevListener, EvdevMode};
use quick_translate_platform::shortcut::portal::ActivationStream;
use quick_translate_platform::shortcut::{Activation, Mechanism};
use quick_translate_secrets::SecretStore;
use quick_translate_ui::shell::{ShellCommand, ShellEvent};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::backend::{self, secret_id};
use super::core::{Backend, Popups};
use super::persist::Change;
use crate::instance::Instance;
use crate::settings::{self, Opened, SettingsWindow};

type ShellUi = mpsc::UnboundedSender<ShellCommand>;

/// Messages from tasks the service spawned.
enum Internal {
    /// A rebuilt backend; only the newest generation is applied.
    Backend(u64, Backend),
}

pub struct Service {
    pub popups: Popups<ShellUi>,
    pub shell_events: mpsc::UnboundedReceiver<ShellEvent>,
    pub inbox: mpsc::Receiver<DaemonCommand>,
    pub instance: Instance,
    pub caps: Capabilities,
    pub shell_label: &'static str,
    pub config_path: PathBuf,
    /// Malformed-config message shown in popups and `--status` until settings are saved.
    pub config_notice: Option<String>,
    /// The portal flow still running in the background.
    pub shortcut: Option<JoinHandle<Mechanism>>,
    /// The mechanism when it was known at startup (no portal).
    pub mechanism: Option<Mechanism>,
    pub color_scheme: BoxStream<'static, ColorScheme>,
    pub secrets: Arc<SecretStore>,
    pub http: HttpClient,
    pub persist: mpsc::UnboundedSender<Change>,
}

struct State {
    mechanism: Option<Arc<Mechanism>>,
    activations: Option<ActivationStream>,
    trigger_changes: Option<BoxStream<'static, Option<String>>>,
    evdev: Option<EvdevListener>,
    evdev_error: Option<String>,
    settings: SettingsWindow,
    internal_tx: mpsc::UnboundedSender<Internal>,
    backend_generation: u64,
}

async fn next_of<S: futures::Stream + Unpin>(stream: &mut Option<S>) -> Option<S::Item> {
    match stream {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

async fn next_evdev(listener: &mut Option<EvdevListener>) -> Option<Activation> {
    match listener {
        Some(listener) => listener.next().await,
        None => std::future::pending().await,
    }
}

impl Service {
    /// Runs until `Quit`, a signal, loss of the bus name, or `stop` (the shell exited).
    pub async fn run(mut self, stop: CancellationToken) {
        let (internal_tx, mut internal) = mpsc::unbounded_channel();
        let mut state = State {
            mechanism: self.mechanism.take().map(Arc::new),
            activations: None,
            trigger_changes: None,
            evdev: None,
            evdev_error: None,
            settings: SettingsWindow::default(),
            internal_tx,
            backend_generation: 0,
        };
        self.popups.set_notice(self.config_notice.clone());
        if self.popups.config().evdev_enabled
            && let Err(err) = self.set_evdev(&mut state, true)
        {
            tracing::warn!("input-device fallback not started: {err}");
        }
        let mut shortcut = self.shortcut.take();
        let (mut sigterm, mut sigint) = match (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) {
            (Ok(term), Ok(int)) => (term, int),
            _ => {
                tracing::error!("cannot install signal handlers");
                return self.shutdown(state, shortcut).await;
            }
        };
        tracing::info!("daemon ready");
        loop {
            tokio::select! {
                command = self.inbox.recv() => match command {
                    Some(DaemonCommand::Quit) | None => {
                        tracing::info!("quit requested");
                        break;
                    }
                    Some(command) => self.command(&mut state, command),
                },
                Some(event) = self.shell_events.recv() => self.shell_event(&mut state, event),
                Some(activation) = next_of(&mut state.activations) => {
                    self.popups.toggle(activation.token, Instant::now());
                }
                Some(_) = next_evdev(&mut state.evdev) => self.popups.toggle(None, Instant::now()),
                Some(trigger) = next_of(&mut state.trigger_changes) => {
                    tracing::info!(?trigger, "portal shortcut changed");
                    if let Some(connection) = self.instance.connection().cloned() {
                        tokio::spawn(async move {
                            let emitted =
                                quick_translate_ipc::emit_shortcut_changed(&connection, trigger.as_deref())
                                    .await;
                            if let Err(err) = emitted {
                                tracing::debug!(%err, "cannot emit ShortcutChanged");
                            }
                        });
                    }
                }
                Some(scheme) = self.color_scheme.next() => {
                    tracing::debug!(?scheme, "color scheme changed");
                    self.popups.set_theme(quick_translate_ui::theme::for_scheme(scheme));
                }
                Some(Internal::Backend(generation, backend)) = internal.recv() => {
                    if generation != state.backend_generation {
                        continue;
                    }
                    tracing::debug!(backend = backend.info.label, configured = backend.info.configured, "backend ready");
                    self.popups.set_backend(backend);
                }
                result = async { shortcut.as_mut().expect("guarded").await }, if shortcut.is_some() => {
                    shortcut = None;
                    match result {
                        Ok(mut mechanism) => {
                            if let Mechanism::Portal(portal) = &mut mechanism {
                                state.activations = portal.take_activations();
                                state.trigger_changes = Some(portal.trigger_changes().boxed());
                            }
                            tracing::info!(mechanism = mechanism.label(), "shortcut ready");
                            state.mechanism = Some(Arc::new(mechanism));
                        }
                        Err(err) => tracing::error!("shortcut setup failed: {err}"),
                    }
                }
                () = self.instance.lost() => {
                    tracing::warn!("lost the session bus or the bus name, exiting");
                    break;
                }
                _ = sigterm.recv() => break,
                _ = sigint.recv() => break,
                () = stop.cancelled() => break,
            }
        }
        self.shutdown(state, shortcut).await;
    }

    async fn shutdown(self, mut state: State, shortcut: Option<JoinHandle<Mechanism>>) {
        let Self {
            mut popups,
            instance,
            ..
        } = self;
        popups.quit();
        state.evdev = None;
        if let Some(shortcut) = shortcut {
            shortcut.abort();
        }
        if let Some(mechanism) = state.mechanism.take()
            && let Ok(Mechanism::Portal(portal)) = Arc::try_unwrap(mechanism)
            && let Err(err) = portal.close().await
        {
            tracing::debug!("closing the portal session failed: {err}");
        }
        instance.release().await;
        tracing::info!("daemon stopped");
    }

    fn command(&mut self, state: &mut State, command: DaemonCommand) {
        match command {
            DaemonCommand::Toggle { options } => {
                self.popups.toggle(options.activation_token, Instant::now());
            }
            DaemonCommand::Show { options } => {
                self.popups.show(options.activation_token, Instant::now());
            }
            DaemonCommand::Settings => self.open_settings(state),
            DaemonCommand::Quit => {}
            DaemonCommand::Status { reply } => {
                let _ = reply.send(self.status(state));
            }
            DaemonCommand::ReloadConfig { reply } => {
                let _ = reply.send(self.reload_config(state));
            }
            DaemonCommand::SetSessionSecret { backend, key } => {
                let id = secret_id(backend);
                if key.is_empty() {
                    self.secrets.clear_session(id);
                } else {
                    self.secrets.set_for_session(id, key.expose());
                }
                self.rebuild_backend(state);
            }
            DaemonCommand::ShortcutInfo { reply } => {
                let _ = reply.send(self.shortcut_info(state));
            }
            DaemonCommand::ConfigureShortcut {
                activation_token,
                reply,
            } => match state.mechanism.clone() {
                Some(mechanism) if matches!(*mechanism, Mechanism::Portal(_)) => {
                    tokio::spawn(async move {
                        let Mechanism::Portal(portal) = &*mechanism else {
                            return;
                        };
                        let result = portal
                            .configure(activation_token)
                            .await
                            .map_err(|err| err.to_string());
                        let _ = reply.send(result);
                    });
                }
                Some(_) => {
                    let _ = reply.send(Err(
                        "The shortcut is a compositor binding; change it in your compositor \
                         configuration."
                            .to_owned(),
                    ));
                }
                None => {
                    let _ = reply.send(Err(
                        "The system shortcut is still being set up; try again in a moment."
                            .to_owned(),
                    ));
                }
            },
            DaemonCommand::SetEvdevEnabled { enabled, reply } => {
                let _ = reply.send(self.set_evdev(state, enabled));
            }
        }
    }

    fn shell_event(&mut self, state: &mut State, event: ShellEvent) {
        match event {
            ShellEvent::OpenSettings => self.open_settings(state),
            ShellEvent::Shown { session } => tracing::debug!(session = session.0, "popup shown"),
            event => {
                if let Some(change) = self.popups.shell_event(event) {
                    let _ = self.persist.send(Change::Popup(change));
                }
            }
        }
    }

    fn status(&self, state: &State) -> DaemonStatus {
        let row = |label: &str, value: &str| format!("\n{:<50} {value}", format!("{label}:"));
        let mut report = self.caps.to_string();
        report.push_str(&row("Popup shell", self.shell_label));
        let shortcut = match state.mechanism.as_deref() {
            Some(mechanism @ Mechanism::Portal(portal)) => format!(
                "{} v{}, trigger {}",
                mechanism.label(),
                portal.version(),
                portal.current_trigger().as_deref().unwrap_or("unassigned")
            ),
            Some(mechanism) => mechanism.label().to_owned(),
            None => "setting up".to_owned(),
        };
        report.push_str(&row("Shortcut", &shortcut));
        let evdev = match (&state.evdev, &state.evdev_error) {
            (Some(_), _) => "on".to_owned(),
            (None, Some(err)) => format!("off ({err})"),
            (None, None) => "off".to_owned(),
        };
        report.push_str(&row("Input-device fallback", &evdev));
        if let Some(notice) = &self.config_notice {
            report.push_str(&row("Config", notice));
        }
        DaemonStatus {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            pid: std::process::id(),
            capabilities: report,
            popup_visible: self.popups.is_visible(),
        }
    }

    fn shortcut_info(&self, state: &State) -> ShortcutInfo {
        match state.mechanism.as_deref() {
            Some(mechanism) => {
                settings::shortcut_info(mechanism, state.evdev.is_some(), state.evdev_error.clone())
            }
            None => ShortcutInfo {
                mechanism: ShortcutMechanism::Portal,
                label: "Setting up the system shortcut…".to_owned(),
                portal_version: self.caps.portal_global_shortcuts.unwrap_or(0),
                evdev_active: state.evdev.is_some(),
                evdev_error: state.evdev_error.clone(),
                evdev_warning: quick_translate_platform::shortcut::evdev::WARNING.to_owned(),
                ..ShortcutInfo::default()
            },
        }
    }

    fn open_settings(&self, state: &mut State) {
        match state.settings.open() {
            Ok(Opened::Spawned) => tracing::info!("settings window started"),
            Ok(Opened::AlreadyOpen) => {
                if let Some(connection) = self.instance.connection().cloned() {
                    tokio::spawn(async move {
                        if let Err(err) =
                            quick_translate_ipc::emit_settings_requested(&connection).await
                        {
                            tracing::debug!(%err, "cannot emit SettingsRequested");
                        }
                    });
                }
            }
            Err(err) => tracing::error!("cannot open the settings window: {err}"),
        }
    }

    /// Settings saved `config.toml`: apply it and re-run the desktop integration install.
    fn reload_config(&mut self, state: &mut State) -> Result<(), String> {
        let config = match ConfigStore::new(&self.config_path).load() {
            LoadOutcome::Loaded(config) | LoadOutcome::Missing(config) => config,
            LoadOutcome::Malformed { error, .. } => {
                return Err(format!("config.toml is malformed: {error}"));
            }
        };
        self.config_notice = None;
        self.popups.set_notice(None);
        if config.evdev_enabled != state.evdev.is_some()
            && let Err(err) = self.set_evdev(state, config.evdev_enabled)
        {
            tracing::warn!("input-device fallback not started: {err}");
        }
        let installed = integration::IntegrationPaths::from_env().and_then(|paths| {
            integration::install(
                &paths,
                &integration::exec_path_from_env()?,
                config.autostart,
            )
        });
        self.popups.set_config(config);
        self.rebuild_backend(state);
        installed
            .map(drop)
            .map_err(|err| format!("cannot update the desktop integration: {err}"))
    }

    fn rebuild_backend(&self, state: &mut State) {
        state.backend_generation += 1;
        let generation = state.backend_generation;
        let config = self.popups.config().clone();
        let secrets = self.secrets.clone();
        let http = self.http.clone();
        let persist = self.persist.clone();
        let tx = state.internal_tx.clone();
        tokio::spawn(async move {
            let backend = backend::build(&config, &secrets, &http, &persist).await;
            let _ = tx.send(Internal::Backend(generation, backend));
        });
    }

    fn set_evdev(&self, state: &mut State, enabled: bool) -> Result<(), String> {
        if !enabled {
            state.evdev = None;
            state.evdev_error = None;
            return Ok(());
        }
        if state.evdev.is_some() {
            return Ok(());
        }
        // Reached only after settings showed the warning and the user confirmed, or from the
        // persisted opt-in that was saved after that confirmation.
        match EvdevListener::start(EvdevMode::Enabled(Confirmed::user_accepted_warning())) {
            Ok(listener) => {
                state.evdev = Some(listener);
                state.evdev_error = None;
                Ok(())
            }
            Err(err) => {
                let message = err.to_string();
                state.evdev_error = Some(message.clone());
                Err(message)
            }
        }
    }
}
