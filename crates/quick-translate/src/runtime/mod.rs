//! `--daemon`: the resident process (design D1, D2, D10).
//!
//! Startup order: Wayland capability probe, one shared Tokio runtime, single-instance claim,
//! config, portal version on a throwaway connection, desktop integration (before any portal
//! registration), data-control thread, HTTP client and backend, color scheme. The portal
//! shortcut setup runs in the background because KDE may hold `BindShortcuts` open behind its
//! first-use dialog. The popup shell then owns the main thread until quit; the service loop
//! runs on the runtime.

mod backend;
mod core;
mod persist;
mod service;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use futures::StreamExt as _;
use futures::future::BoxFuture;
use quick_translate_backends::build_client;
use quick_translate_core::{Capabilities, Compositor, ConfigStore, LoadOutcome};
use quick_translate_platform::outputs::OutputTracker;
use quick_translate_platform::selection::{self, Captured, DataControlSource, STEP_TIMEOUT};
use quick_translate_platform::shortcut::{self, ToggleCommand, portal};
use quick_translate_platform::wayland::{self, DataControlHandle, WaylandDisplay};
use quick_translate_platform::{appearance, integration, sway};
use quick_translate_secrets::SecretStore;
use quick_translate_ui::shell::{ShellCommand, ShellEvent};
use quick_translate_ui::{executor, layer, shell, theme, window};
use tokio_util::sync::CancellationToken;

use self::core::{Capture, Popups};
use self::service::Service;
use crate::instance;

/// Which popup shell runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellKind {
    Layer,
    Window,
    /// `QT_HEADLESS=1`: no Wayland and no popup surfaces, for the CLI and IPC tests.
    Headless,
}

impl ShellKind {
    /// Layer shell when advertised. `QT_POPUP_SHELL=window` forces the window shell (testing).
    fn choose(caps: &Capabilities) -> Self {
        let forced = std::env::var("QT_POPUP_SHELL").ok();
        match forced.as_deref() {
            Some("window") => Self::Window,
            Some("layer") if caps.has_layer_shell() => Self::Layer,
            _ if caps.has_layer_shell() => Self::Layer,
            _ => Self::Window,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Layer => "layer (overlay per output)",
            Self::Window => "window (xdg-toplevel)",
            Self::Headless => "none (headless)",
        }
    }
}

enum Launch {
    Layer(layer::LayerShell),
    Window(window::WindowShell),
    Headless(shell::ShellEnd),
}

/// Sway only sends `wl_pointer.enter` on motion; a zero-length cursor move over its IPC makes it
/// send the position to the overlay under the resting pointer (spike 2.1). No key, text or
/// button input is sent.
struct SwayNudge {
    socket: std::path::PathBuf,
}

impl layer::PointerNudge for SwayNudge {
    fn nudge(&self) {
        let socket = self.socket.clone();
        let spawned = std::thread::Builder::new()
            .name("qt-sway-nudge".into())
            .spawn(move || {
                match sway::run_command(&socket, sway::CURSOR_REBASE, Duration::from_millis(500)) {
                    Ok(_) => tracing::debug!("sway cursor rebase sent"),
                    Err(err) => tracing::debug!("sway cursor rebase failed: {err}"),
                }
            });
        if let Err(err) = spawned {
            tracing::debug!("cannot start the sway nudge: {err}");
        }
    }
}

/// Answers shell commands without any surface: Show is shown at once, Hide closes.
async fn headless_shell(shell: shell::ShellEnd) {
    let Some(mut commands) = shell.commands.take() else {
        return;
    };
    let mut open = None;
    while let Some(command) = commands.recv().await {
        match command {
            ShellCommand::Show(request) => {
                if let Some(session) = open.replace(request.session) {
                    shell.send(ShellEvent::Closed { session });
                }
                shell.send(ShellEvent::Shown {
                    session: request.session,
                });
            }
            ShellCommand::Hide => {
                if let Some(session) = open.take() {
                    shell.send(ShellEvent::Closed { session });
                }
            }
            ShellCommand::Quit => break,
            ShellCommand::Captured { .. } | ShellCommand::SetTheme(_) => {}
        }
    }
}

/// Data-control CLIPBOARD, then data-control PRIMARY, each with the 150 ms step budget.
struct DataControlCapture {
    primary: DataControlSource,
    clipboard: DataControlSource,
}

impl Capture for DataControlCapture {
    fn capture(&self) -> BoxFuture<'static, Captured> {
        let primary = self.primary.clone();
        let clipboard = self.clipboard.clone();
        Box::pin(async move { selection::capture(&[&clipboard, &primary], STEP_TIMEOUT).await })
    }
}

pub fn run() -> ExitCode {
    crate::logging::init();
    let started = Instant::now();
    let headless = std::env::var_os("QT_HEADLESS").is_some_and(|v| v == "1");
    let (display, mut caps) = if headless {
        (None, Capabilities::default())
    } else {
        let probed = WaylandDisplay::from_env().and_then(|display| {
            let caps = wayland::probe_display(&display)?;
            Ok((display, caps))
        });
        match probed {
            Ok((display, caps)) => (Some(display), caps),
            Err(err) => {
                eprintln!("quick-translate: {err}");
                return ExitCode::FAILURE;
            }
        }
    };
    caps.compositor = Compositor::detect();
    let kind = if headless {
        ShellKind::Headless
    } else {
        ShellKind::choose(&caps)
    };
    prefer_software_renderer();

    // A resident daemon with light async work: a couple of workers are plenty.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("qt-runtime")
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("quick-translate: cannot start the async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    executor::install(runtime.handle().clone());

    let (service, launch) = match runtime.block_on(startup(display, caps, kind)) {
        Ok(Some(started)) => started,
        Ok(None) => {
            tracing::info!("another quick-translate daemon is already running");
            return ExitCode::SUCCESS;
        }
        Err(err) => {
            tracing::error!("{err:#}");
            eprintln!("quick-translate: {err:#}");
            return ExitCode::FAILURE;
        }
    };
    tracing::debug!(ms = started.elapsed().as_millis() as u64, "daemon started");

    let stop = CancellationToken::new();
    let service = runtime.spawn(service.run(stop.clone()));
    let shell = match launch {
        Launch::Layer(settings) => layer::run(settings).map_err(|e| e.to_string()),
        Launch::Window(settings) => window::run(settings).map_err(|e| e.to_string()),
        Launch::Headless(shell) => {
            runtime.block_on(headless_shell(shell));
            Ok(())
        }
    };
    stop.cancel();
    if let Err(err) = runtime.block_on(service) {
        tracing::error!("daemon task failed: {err}");
    }
    runtime.shutdown_timeout(Duration::from_secs(1));
    match shell {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("popup shell failed: {err}");
            eprintln!("quick-translate: the popup shell failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// The popup renders with tiny-skia unless `ICED_BACKEND` says otherwise (design D4: the
/// software renderer is the fallback when the GPU renderer exceeds the 80 MB idle budget).
/// Measured on the dev machine (NVIDIA, release, idle 60 s): wgpu 233 MB RSS and 0.18 % CPU from
/// a driver thread, tiny-skia 24 MB and 0.00 %; first frames were comparable (40–60 ms). The
/// window shell also needs it because iced drops its renderer with the last window, so wgpu
/// would be set up again for every popup.
#[allow(unsafe_code)]
fn prefer_software_renderer() {
    if std::env::var_os("ICED_BACKEND").is_none() {
        // SAFETY: runs on the main thread before the runtime, the toolkit or any other thread
        // exists, so nothing can read the environment concurrently.
        unsafe { std::env::set_var("ICED_BACKEND", "tiny-skia") };
    }
}

async fn startup(
    display: Option<WaylandDisplay>,
    mut caps: Capabilities,
    kind: ShellKind,
) -> anyhow::Result<Option<(Service, Launch)>> {
    let (commands, inbox) = tokio::sync::mpsc::channel(32);
    let Some(instance) = instance::claim(commands).await? else {
        return Ok(None);
    };

    let config_path = ConfigStore::default_path();
    let (config, config_notice) = match ConfigStore::new(&config_path).load() {
        LoadOutcome::Loaded(config) | LoadOutcome::Missing(config) => (config, None),
        LoadOutcome::Malformed {
            config,
            path,
            error,
        } => {
            let notice = format!(
                "{} is malformed, using defaults until settings are saved: {error}",
                path.display()
            );
            tracing::warn!("{notice}");
            (config, Some(notice))
        }
    };

    // A portal call before Registry.Register would tie that connection to a guessed app ID, so
    // the version is read on a connection that is closed right away.
    if let Ok(probe) = zbus::Connection::session().await {
        caps.portal_global_shortcuts = portal::detect_version(&probe).await;
        drop(probe);
    }
    tracing::info!(
        layer_shell = caps.layer_shell,
        data_control = ?caps.data_control(),
        portal_global_shortcuts = caps.portal_global_shortcuts,
        compositor = %caps.compositor,
        shell = kind.label(),
        "capabilities"
    );

    // The portal resolves the app ID through the installed .desktop file.
    match integration::IntegrationPaths::from_env().and_then(|paths| {
        integration::install(
            &paths,
            &integration::exec_path_from_env()?,
            config.autostart,
        )
    }) {
        Ok(changes) => {
            for path in &changes.written {
                tracing::info!(path = %path.display(), "installed");
            }
            for path in &changes.removed {
                tracing::info!(path = %path.display(), "removed");
            }
        }
        Err(err) => tracing::warn!("cannot install the desktop integration: {err}"),
    }

    // BindShortcuts may wait for KDE's first-use dialog, so the portal flow runs in the
    // background. Without a portal the compositor-binding mechanism is known at once.
    let (shortcut, mechanism) = if caps.portal_global_shortcuts.is_some() {
        let portal_connection = zbus::Connection::session().await.ok();
        let shortcut_caps = caps.clone();
        let task = tokio::spawn(async move {
            shortcut::start(
                portal_connection.as_ref(),
                &shortcut_caps,
                &ToggleCommand::detect(),
            )
            .await
        });
        (Some(task), None)
    } else {
        let mechanism = shortcut::start(None, &caps, &ToggleCommand::detect()).await;
        (None, Some(mechanism))
    };

    // `QT_SELECTION=focus` skips data-control and reads on focus like on GNOME (testing).
    let force_focus = std::env::var("QT_SELECTION").is_ok_and(|v| v == "focus");
    let protocol = caps.data_control().filter(|_| !force_focus);
    let capture: Option<Arc<dyn Capture>> = match (protocol, &display) {
        (Some(protocol), Some(display)) => match DataControlHandle::spawn_on(display, protocol) {
            Ok(handle) => Some(Arc::new(DataControlCapture {
                primary: DataControlSource::primary(handle.clone()),
                clipboard: DataControlSource::clipboard(handle),
            })),
            Err(err) => {
                tracing::warn!("data-control unavailable, reading the selection on focus: {err}");
                None
            }
        },
        _ => None,
    };

    let persist = persist::spawn(config_path.clone());
    let http = build_client(Duration::from_secs(config.request_timeout_secs.max(1)))
        .map_err(|err| anyhow::anyhow!("cannot create the HTTP client: {err}"))?;
    let secrets = Arc::new(SecretStore::connect().await);
    let backend = backend::build(&config, &secrets, &http, &persist).await;

    let (scheme, color_scheme) = match zbus::Connection::session().await {
        Ok(connection) => appearance::watch(&connection).await,
        Err(_) => (Default::default(), futures::stream::pending().boxed()),
    };
    let theme = theme::for_scheme(scheme);

    let (daemon_end, shell_end) = shell::channel();
    let launch = match (kind, display) {
        (ShellKind::Layer, Some(display)) => {
            // Without data-control the shell reads the selection through the toolkit, which
            // needs the toolkit's own connection (keyboard focus is per connection).
            let focus_capture = capture.is_none();
            let connection = if focus_capture {
                Some(
                    display
                        .connect()
                        .context("cannot connect the popup shell")?,
                )
            } else {
                None
            };
            Launch::Layer(layer::LayerShell {
                shell: shell_end,
                theme: theme.clone(),
                outputs: OutputTracker::spawn_on(&display).context("cannot enumerate outputs")?,
                connection,
                nudge: sway::socket_from_env()
                    .map(|socket| Arc::new(SwayNudge { socket }) as Arc<dyn layer::PointerNudge>),
                focus_capture,
            })
        }
        (ShellKind::Window, _) => Launch::Window(window::WindowShell {
            shell: shell_end,
            theme: theme.clone(),
        }),
        (ShellKind::Layer | ShellKind::Headless, _) => Launch::Headless(shell_end),
    };
    let popups = Popups::new(daemon_end.commands, capture, backend, config, theme);
    let service = Service {
        popups,
        shell_events: daemon_end.events,
        inbox,
        instance,
        caps,
        shell_label: kind.label(),
        config_path,
        config_notice,
        shortcut,
        mechanism,
        color_scheme,
        secrets,
        http,
        persist,
    };
    Ok(Some((service, launch)))
}
