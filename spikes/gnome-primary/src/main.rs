//! Week-1 spike 2.5: focus-time PRIMARY read (the D5 path for compositors without
//! data-control, i.e. GNOME/Mutter).
//!
//! A plain winit 0.30 window (the winit iced 0.14 uses) is created with an xdg-activation
//! token, waits for keyboard focus, then reads PRIMARY through smithay-clipboard 0.7 (the
//! crate iced's `clipboard::Kind::Primary` ends up in on Wayland), prints timings and exits.
//!
//! Modes:
//!   (default)  token from --token or $XDG_ACTIVATION_TOKEN; one window; exit.
//!              This is the "GNOME custom shortcut runs the CLI" path.
//!   --portal   in-process GlobalShortcuts portal (Register, CreateSession, BindShortcuts);
//!              each Activated opens a window with the Activated `activation_token`.
//!              This is the "portal trigger" path, as the real daemon would do it.
//!   --cleanup  remove this spike's .desktop file.
//!
//! Throwaway code. Not part of the product.

use std::{
    num::NonZeroU32,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    platform::{
        startup_notify::WindowAttributesExtStartupNotify,
        wayland::{EventLoopBuilderExtWayland, WindowAttributesExtWayland},
    },
    raw_window_handle::{HasDisplayHandle, RawDisplayHandle},
    window::{ActivationToken, Window, WindowId},
};

const APP_ID: &str = "io.github.quicktranslate.QuickTranslate.PrimarySpike";

// ---------------------------------------------------------------- options

struct Opts {
    portal: bool,
    cleanup: bool,
    token: Option<String>,
    timeout: Duration,
    budget: Duration,
    listen: Duration,
    count: u32,
    trigger: String,
    redact: bool,
    clipboard_too: bool,
}

fn parse_opts() -> Result<Opts> {
    let mut o = Opts {
        portal: false,
        cleanup: false,
        token: None,
        timeout: Duration::from_secs(3),
        budget: Duration::from_millis(500),
        listen: Duration::from_secs(120),
        count: 5,
        trigger: "CTRL+ALT+c".into(),
        redact: false,
        clipboard_too: false,
    };
    let mut a = std::env::args().skip(1);
    while let Some(arg) = a.next() {
        match arg.as_str() {
            "--portal" => o.portal = true,
            "--cleanup" => o.cleanup = true,
            "--token" => o.token = Some(a.next().context("--token TOKEN")?),
            "--timeout" => o.timeout = Duration::from_secs_f64(a.next().context("--timeout SECS")?.parse()?),
            "--budget-ms" => o.budget = Duration::from_millis(a.next().context("--budget-ms MS")?.parse()?),
            "--listen" => o.listen = Duration::from_secs(a.next().context("--listen SECS")?.parse()?),
            "--count" => o.count = a.next().context("--count N")?.parse()?,
            "--trigger" => o.trigger = a.next().context("--trigger T")?,
            "--redact" => o.redact = true,
            "--clipboard" => o.clipboard_too = true,
            "--help" | "-h" => {
                println!(
                    "gnome-primary-spike [--token T] [--timeout 3] [--budget-ms 500] [--redact] [--clipboard]\n\
                     gnome-primary-spike --portal [--trigger CTRL+ALT+c] [--count 5] [--listen 120]\n\
                     gnome-primary-spike --cleanup\n\
                     Exit codes (single mode): 0 PRIMARY read, 2 no keyboard focus before --timeout,\n\
                     3 focused but PRIMARY unreadable/empty within --budget-ms."
                );
                std::process::exit(0);
            }
            other => bail!("unknown argument {other:?}"),
        }
    }
    Ok(o)
}

// ---------------------------------------------------------------- desktop file

fn desktop_path() -> Result<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .context("no HOME")?;
    Ok(data.join("applications").join(format!("{APP_ID}.desktop")))
}

fn install_desktop() -> Result<()> {
    let path = desktop_path()?;
    let exe = std::env::current_exe()?;
    let content = format!(
        "[Desktop Entry]\nType=Application\nName=Quick Translate (PRIMARY spike)\n\
         Comment=Throwaway spike; safe to delete\nExec={} --portal\nIcon=accessories-dictionary\n\
         Terminal=false\nNoDisplay=true\n",
        exe.display()
    );
    if std::fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
        std::fs::create_dir_all(path.parent().context("parent")?)?;
        std::fs::write(&path, content)?;
    }
    println!("[desktop] {}", path.display());
    Ok(())
}

// ---------------------------------------------------------------- events

#[derive(Debug)]
enum UserEv {
    PortalStatus(String),
    Activated { token: Option<String>, portal_ts_ms: u64, at: Instant },
    PortalDone,
    Primary { generation: u32, report: String, ok: bool },
    WindowTimeout { generation: u32 },
}

enum ClipCmd {
    Read { focus_at: Instant },
    Exit,
}

struct Win {
    generation: u32,
    window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    _ctx: softbuffer::Context<Arc<Window>>,
    clip_tx: mpsc::Sender<ClipCmd>,
    clip_thread: Option<std::thread::JoinHandle<()>>,
    created: Instant,
    origin: Instant,
    first_frame: bool,
    focused: bool,
    read_requested: bool,
}

impl Drop for Win {
    fn drop(&mut self) {
        let _ = self.clip_tx.send(ClipCmd::Exit);
        if let Some(t) = self.clip_thread.take() {
            let _ = t.join();
        }
    }
}

struct App {
    o: Opts,
    proc_start: Instant,
    proxy: EventLoopProxy<UserEv>,
    single_token: Option<String>,
    win: Option<Win>,
    generation: u32,
    windows_done: u32,
    exit_code: i32,
}

fn rel(origin: Instant) -> String {
    format!("[t+{:7.1}ms]", origin.elapsed().as_secs_f64() * 1e3)
}

fn show_text(s: &str, redact: bool) -> String {
    if redact {
        return format!("<redacted, {} chars>", s.chars().count());
    }
    let short: String = s.chars().take(80).collect();
    format!("{short:?}{}", if s.chars().count() > 80 { "…" } else { "" })
}

impl App {
    fn open_window(&mut self, el: &ActiveEventLoop, token: Option<String>, origin: Instant) {
        self.generation += 1;
        let generation = self.generation;
        let mut attrs = Window::default_attributes()
            .with_title("Quick Translate PRIMARY spike")
            .with_inner_size(winit::dpi::LogicalSize::new(420.0, 140.0))
            .with_name(APP_ID, "");
        let has_token = token.is_some();
        if let Some(t) = token {
            attrs = attrs.with_activation_token(ActivationToken::from_raw(t));
        }
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                println!("{} create_window failed: {e}", rel(origin));
                self.exit_code = 1;
                el.exit();
                return;
            }
        };
        println!(
            "{} window #{generation} created, activation token {}",
            rel(origin),
            if has_token { "PASSED to xdg_activation_v1.activate" } else { "ABSENT (no activate request)" }
        );

        // Same as iced: one clipboard per window, built from the window's wl_display.
        let display_ptr = match window.display_handle().map(|h| h.as_raw()) {
            Ok(RawDisplayHandle::Wayland(h)) => h.display.as_ptr() as usize,
            other => {
                println!("not a Wayland display handle: {other:?}");
                self.exit_code = 1;
                el.exit();
                return;
            }
        };
        // SAFETY: the wl_display outlives the clipboard; Win::drop joins the clipboard thread
        // before the window/event loop go away.
        #[allow(unsafe_code)]
        let clipboard = unsafe { smithay_clipboard::Clipboard::new(display_ptr as *mut _) };
        let (clip_tx, clip_rx) = mpsc::channel::<ClipCmd>();
        let proxy = self.proxy.clone();
        let budget = self.o.budget;
        let redact = self.o.redact;
        let clipboard_too = self.o.clipboard_too;
        let clip_thread = std::thread::spawn(move || {
            while let Ok(cmd) = clip_rx.recv() {
                let ClipCmd::Read { focus_at } = cmd else { break };
                let deadline = focus_at + budget;
                let mut attempts = 0u32;
                let mut errors: Vec<String> = Vec::new();
                let (report, ok) = loop {
                    attempts += 1;
                    let t = Instant::now();
                    // NB: load_primary blocks on an unbounded recv() until the source client
                    // writes the data and closes the pipe; there is no timeout inside it.
                    match clipboard.load_primary() {
                        Ok(text) if !text.trim().is_empty() => {
                            break (
                                format!(
                                    "PRIMARY OK: attempt {attempts}, {:.1}ms after focus (this read {:.1}ms), {} chars: {}",
                                    focus_at.elapsed().as_secs_f64() * 1e3,
                                    t.elapsed().as_secs_f64() * 1e3,
                                    text.chars().count(),
                                    show_text(&text, redact)
                                ),
                                true,
                            );
                        }
                        Ok(_) => {
                            if errors.last().map(String::as_str) != Some("whitespace-only/empty text") {
                                errors.push("whitespace-only/empty text".into());
                            }
                        }
                        Err(e) => {
                            let e = e.to_string();
                            if errors.last() != Some(&e) {
                                errors.push(e);
                            }
                        }
                    }
                    if Instant::now() >= deadline {
                        break (
                            format!(
                                "PRIMARY FAILED after {attempts} attempts / {:.0}ms; distinct errors in order: {errors:?}",
                                focus_at.elapsed().as_secs_f64() * 1e3
                            ),
                            false,
                        );
                    }
                    std::thread::sleep(Duration::from_millis(2));
                };
                let mut report = report;
                if !errors.is_empty() && ok {
                    report.push_str(&format!(" (errors before success: {errors:?})"));
                }
                if clipboard_too {
                    match clipboard.load() {
                        Ok(t) => report.push_str(&format!("\n             CLIPBOARD: {} chars: {}", t.chars().count(), show_text(&t, redact))),
                        Err(e) => report.push_str(&format!("\n             CLIPBOARD: error {e}")),
                    }
                }
                let _ = proxy.send_event(UserEv::Primary { generation, report, ok });
            }
            drop(clipboard);
        });

        let ctx = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface = softbuffer::Surface::new(&ctx, window.clone()).expect("softbuffer surface");
        window.request_redraw();

        let proxy = self.proxy.clone();
        let timeout = self.o.timeout;
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            let _ = proxy.send_event(UserEv::WindowTimeout { generation });
        });

        self.win = Some(Win {
            generation,
            window,
            surface,
            _ctx: ctx,
            clip_tx,
            clip_thread: Some(clip_thread),
            created: Instant::now(),
            origin,
            first_frame: false,
            focused: false,
            read_requested: false,
        });
    }

    fn close_window(&mut self, el: &ActiveEventLoop, code: i32) {
        self.win = None;
        self.windows_done += 1;
        if !self.o.portal {
            self.exit_code = code;
            el.exit();
        } else if self.windows_done >= self.o.count {
            println!("[portal] {} activations handled, exiting", self.windows_done);
            el.exit();
        }
    }
}

impl ApplicationHandler<UserEv> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if !self.o.portal && self.win.is_none() && self.windows_done == 0 {
            let token = self.single_token.take();
            let origin = self.proc_start;
            self.open_window(el, token, origin);
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: UserEv) {
        match ev {
            UserEv::PortalStatus(s) => println!("{s}"),
            UserEv::PortalDone => {
                println!("[portal] listener finished");
                el.exit();
            }
            UserEv::Activated { token, portal_ts_ms, at } => {
                println!(
                    "\n{} ACTIVATED portal_ts={portal_ts_ms}ms activation_token={}",
                    rel(at),
                    token.as_deref().map_or("ABSENT".to_owned(), |t| format!("PRESENT(len={})", t.len()))
                );
                if self.win.is_some() {
                    println!("  window already open; closing it (toggle semantics)");
                    self.win = None;
                    return;
                }
                self.open_window(el, token, at);
            }
            UserEv::Primary { generation, report, ok } => {
                let Some(w) = &self.win else { return };
                if w.generation != generation {
                    return;
                }
                println!("{} {report}", rel(w.origin));
                self.close_window(el, if ok { 0 } else { 3 });
            }
            UserEv::WindowTimeout { generation } => {
                let Some(w) = &self.win else { return };
                if w.generation != generation {
                    return;
                }
                if !w.focused {
                    println!(
                        "{} TIMEOUT: keyboard focus NOT granted within {:?} (first frame presented: {})",
                        rel(w.origin),
                        self.o.timeout,
                        w.first_frame
                    );
                    self.close_window(el, 2);
                } else {
                    println!("{} TIMEOUT while reading PRIMARY", rel(w.origin));
                    self.close_window(el, 3);
                }
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(w) = self.win.as_mut() else { return };
        match event {
            WindowEvent::RedrawRequested => {
                let size = w.window.inner_size();
                if let (Some(width), Some(height)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
                    if w.surface.resize(width, height).is_ok() {
                        if let Ok(mut buf) = w.surface.buffer_mut() {
                            let color = if w.focused { 0x002f_6f4f } else { 0x0030_3040 };
                            buf.fill(color);
                            let _ = buf.present();
                            if !w.first_frame {
                                w.first_frame = true;
                                println!(
                                    "{} first frame presented ({:.1}ms after create_window)",
                                    rel(w.origin),
                                    w.created.elapsed().as_secs_f64() * 1e3
                                );
                            }
                        }
                    }
                }
            }
            WindowEvent::Focused(f) => {
                println!("{} Focused({f})", rel(w.origin));
                w.focused = f;
                w.window.request_redraw();
                if f && !w.read_requested {
                    w.read_requested = true;
                    let _ = w.clip_tx.send(ClipCmd::Read { focus_at: Instant::now() });
                }
            }
            WindowEvent::CloseRequested => {
                println!("{} close requested", rel(w.origin));
                self.close_window(el, 4);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- portal thread

fn spawn_portal(o: &Opts, proxy: EventLoopProxy<UserEv>) {
    let trigger = o.trigger.clone();
    let listen = o.listen;
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("rt");
        let p = proxy.clone();
        let r: Result<()> = rt.block_on(async move {
            use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
            use futures_util::StreamExt;
            let say = |s: String| {
                let _ = p.send_event(UserEv::PortalStatus(s));
            };
            install_desktop()?;
            let conn = zbus::Connection::session().await?;
            let app_id = ashpd::AppID::try_from(APP_ID).map_err(|e| anyhow::anyhow!("{e}"))?;
            ashpd::register_host_app_with_connection(conn.clone(), app_id).await?;
            say(format!("[portal] Register({APP_ID}) OK"));
            let gs = GlobalShortcuts::with_connection(conn.clone()).await?;
            say(format!("[portal] GlobalShortcuts version {}", gs.version()));
            let session = gs.create_session(Default::default()).await?;
            let mut activated = gs.receive_activated().await?;
            say(format!("[portal] BindShortcuts(translate-selection, {trigger}) — accept the dialog if shown"));
            let bound = gs
                .bind_shortcuts(
                    &session,
                    &[NewShortcut::new("translate-selection", "Translate selection (PRIMARY spike)")
                        .preferred_trigger(trigger.as_str())],
                    None,
                    Default::default(),
                )
                .await?
                .response()?;
            for s in bound.shortcuts() {
                say(format!("[portal] bound {} -> {:?}", s.id(), s.trigger_description()));
            }
            say(format!("[portal] select text in another app, then press the shortcut (listening {listen:?})"));
            let end = tokio::time::sleep(listen);
            tokio::pin!(end);
            loop {
                tokio::select! {
                    _ = &mut end => break,
                    Some(a) = activated.next() => {
                        let at = Instant::now();
                        let token = a.options().get("activation_token")
                            .and_then(|v| <&str>::try_from(&**v).ok().map(str::to_owned));
                        if p.send_event(UserEv::Activated { token, portal_ts_ms: a.timestamp().as_millis() as u64, at }).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = session.close().await;
            Ok(())
        });
        if let Err(e) = r {
            let _ = proxy.send_event(UserEv::PortalStatus(format!("[portal] ERROR: {e:#}")));
        }
        let _ = proxy.send_event(UserEv::PortalDone);
    });
}

// ---------------------------------------------------------------- main

fn main() -> Result<()> {
    let proc_start = Instant::now();
    let o = parse_opts()?;
    if o.cleanup {
        let p = desktop_path()?;
        match std::fs::remove_file(&p) {
            Ok(()) => println!("removed {}", p.display()),
            Err(e) => println!("{}: {e}", p.display()),
        }
        return Ok(());
    }
    println!(
        "[env] XDG_CURRENT_DESKTOP={:?} WAYLAND_DISPLAY={:?} XDG_ACTIVATION_TOKEN={} DESKTOP_STARTUP_ID={}",
        std::env::var("XDG_CURRENT_DESKTOP").ok(),
        std::env::var("WAYLAND_DISPLAY").ok(),
        if std::env::var_os("XDG_ACTIVATION_TOKEN").is_some() { "PRESENT" } else { "ABSENT" },
        if std::env::var_os("DESKTOP_STARTUP_ID").is_some() { "PRESENT" } else { "ABSENT" },
    );
    let single_token = o.token.clone().or_else(|| std::env::var("XDG_ACTIVATION_TOKEN").ok());
    // Don't leak the token to children (winit's advice).
    #[allow(unsafe_code)]
    // SAFETY: single-threaded at this point.
    unsafe {
        std::env::remove_var("XDG_ACTIVATION_TOKEN");
        std::env::remove_var("DESKTOP_STARTUP_ID");
    }

    let event_loop = EventLoop::<UserEv>::with_user_event().with_wayland().build()?;
    let proxy = event_loop.create_proxy();
    if o.portal {
        spawn_portal(&o, proxy.clone());
    }
    let mut app = App {
        o,
        proc_start,
        proxy,
        single_token,
        win: None,
        generation: 0,
        windows_done: 0,
        exit_code: 0,
    };
    event_loop.run_app(&mut app)?;
    let code = app.exit_code;
    drop(app);
    std::process::exit(code);
}
