//! Week-1 spike 2.4: XDG GlobalShortcuts portal from an unsandboxed binary.
//!
//! Phases (run `--help`):
//!   --probe      non-interactive: Register, version, CreateSession, ListShortcuts, close.
//!   --bind       BindShortcuts(CTRL+ALT+c) (KDE shows a dialog on first bind), then listen
//!                for Activated and print latency data.
//!   --stamp      tiny client for a *native* KDE custom shortcut: reports its own start time
//!                to a running `--bind` listener (the `--toggle` analogue).
//!   --evdev-log  optional root helper: forwards kernel key-down timestamps of shortcut-like
//!                chords (modifier held) to the listener, as ground truth for "key press" time.
//!   --cleanup    remove the .desktop file and the spike's kglobalaccel component.
//!
//! Throwaway code. Not part of the product.

use std::{
    collections::HashMap,
    io::Write as _,
    os::unix::net::UnixDatagram as StdUnixDatagram,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use ashpd::{
    AppID,
    desktop::global_shortcuts::{GlobalShortcuts, NewShortcut},
};
use futures_util::StreamExt;
use rustix::time::{ClockId, clock_gettime};
use zbus::{MatchRule, MessageStream, message::Type as MsgType, zvariant::OwnedObjectPath};

const APP_ID: &str = "io.github.quicktranslate.QuickTranslate.Spike";
const SHORTCUT_ID: &str = "translate-selection";
const DEFAULT_TRIGGER: &str = "CTRL+ALT+c";
const KGA_SERVICE: &str = "org.kde.kglobalaccel";
const KGA_IFACE: &str = "org.kde.KGlobalAccel";
const KGA_COMPONENT_IFACE: &str = "org.kde.kglobalaccel.Component";
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------- clocks

#[derive(Clone, Copy, Debug)]
struct Stamp {
    mono_ns: i128,
    boot_ns: i128,
    real_ns: i128,
}

fn clock_ns(id: ClockId) -> i128 {
    let t = clock_gettime(id);
    i128::from(t.tv_sec) * 1_000_000_000 + i128::from(t.tv_nsec)
}

fn now() -> Stamp {
    Stamp {
        mono_ns: clock_ns(ClockId::Monotonic),
        boot_ns: clock_ns(ClockId::Boottime),
        real_ns: clock_ns(ClockId::Realtime),
    }
}

fn ms(ns: i128) -> f64 {
    ns as f64 / 1e6
}

/// The portal documents the Activated timestamp as "millisecond granularity, with an
/// undefined base". Compare it against every plausible clock and report all of them.
fn interpret_ms_timestamp(ts_ms: i128, at: Stamp) -> String {
    let d = |clock_ns: i128| clock_ns as f64 / 1e6 - ts_ms as f64;
    let (m, b, r) = (d(at.mono_ns), d(at.boot_ns), d(at.real_ns));
    let plausible = |x: f64| (-1.0..5000.0).contains(&x);
    let mut best = Vec::new();
    if plausible(m) {
        best.push("MONOTONIC");
    }
    if plausible(b) {
        best.push("BOOTTIME");
    }
    if plausible(r) {
        best.push("REALTIME");
    }
    format!(
        "now-ts: mono={m:.1}ms boot={b:.1}ms real={r:.1}ms (plausible base: {})",
        if best.is_empty() { "none".to_owned() } else { best.join("/") }
    )
}

// ---------------------------------------------------------------- options

#[derive(Debug, PartialEq)]
enum Mode {
    Probe,
    Bind,
    ListenOnly,
    Cleanup,
    Stamp,
    EvdevLog,
    Help,
}

#[derive(Debug)]
struct Opts {
    mode: Mode,
    listen_secs: u64,
    trigger: String,
    no_register: bool,
    no_desktop: bool,
    configure: bool,
    exec: Option<String>,
    sock: PathBuf,
    devices: Vec<PathBuf>,
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

fn default_sock() -> PathBuf {
    runtime_dir().join("qt-portal-spike.sock")
}

fn stamp_log() -> PathBuf {
    runtime_dir().join("qt-portal-spike-stamps.log")
}

fn parse_opts() -> Result<Opts> {
    let mut o = Opts {
        mode: Mode::Help,
        listen_secs: 60,
        trigger: DEFAULT_TRIGGER.to_owned(),
        no_register: false,
        no_desktop: false,
        configure: false,
        exec: None,
        sock: default_sock(),
        devices: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--probe" => o.mode = Mode::Probe,
            "--bind" => o.mode = Mode::Bind,
            "--listen-only" => o.mode = Mode::ListenOnly,
            "--cleanup" => o.mode = Mode::Cleanup,
            "--stamp" => o.mode = Mode::Stamp,
            "--evdev-log" => o.mode = Mode::EvdevLog,
            "--help" | "-h" => o.mode = Mode::Help,
            "--listen" => {
                o.listen_secs = args.next().context("--listen SECS")?.parse()?;
            }
            "--trigger" => o.trigger = args.next().context("--trigger TRIGGER")?,
            "--no-register" => o.no_register = true,
            "--no-desktop" => o.no_desktop = true,
            "--configure" => o.configure = true,
            "--exec" => o.exec = Some(args.next().context("--exec CMD")?),
            "--sock" => o.sock = PathBuf::from(args.next().context("--sock PATH")?),
            p if o.mode == Mode::EvdevLog && p.starts_with('/') => o.devices.push(PathBuf::from(p)),
            other => bail!("unknown argument {other:?} (see --help)"),
        }
    }
    Ok(o)
}

const HELP: &str = "\
portal-shortcuts-spike — quick-translate week-1 spike 2.4

  --probe [--no-register] [--no-desktop]
        Non-interactive. Installs the spike .desktop file, Registry.Register, reads the
        GlobalShortcuts `version`, CreateSession, ListShortcuts, Session.Close. Never calls
        BindShortcuts, so no dialog. --no-register / --no-desktop demonstrate failure modes.
  --bind [--listen SECS] [--trigger T] [--configure] [--exec CMD]
        Register, CreateSession, ListShortcuts (persisted), BindShortcuts([translate-selection,
        T=CTRL+ALT+c]), ListShortcuts, then print every Activated / Deactivated /
        ShortcutsChanged and every kglobalaccel globalShortcutPressed for SECS seconds
        (0 = until Ctrl+C). --configure calls ConfigureShortcuts (v2) after binding.
        --exec runs `sh -c CMD` on each activation with XDG_ACTIVATION_TOKEN set.
        Also listens on $XDG_RUNTIME_DIR/qt-portal-spike.sock for --stamp / --evdev-log.
  --listen-only [--listen SECS]
        Same listener as --bind (kglobalaccel signals, --stamp, --evdev-log) but makes no
        portal calls at all. Use it to measure a native KDE custom shortcut on its own.
  --stamp [--sock PATH]
        Client for a native KDE custom shortcut: sends its start time to the listener.
  --evdev-log --sock PATH /dev/input/eventN...
        Optional, needs read access to input devices (e.g. sudo). Forwards only key-down
        timestamps of non-modifier keys pressed while Ctrl/Alt/Meta is held.
  --cleanup
        Removes the spike .desktop file, socket, stamp log and the spike's kglobalaccel
        component entries (run after every spike process has exited).
";

// ---------------------------------------------------------------- desktop file

fn desktop_path() -> Result<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .context("no HOME")?;
    Ok(data.join("applications").join(format!("{APP_ID}.desktop")))
}

fn install_desktop() -> Result<PathBuf> {
    let path = desktop_path()?;
    let exe = std::env::current_exe()?;
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Quick Translate (portal spike)\n\
         Comment=Throwaway GlobalShortcuts spike; safe to delete\n\
         Exec={} --bind --listen 60\n\
         Icon=accessories-dictionary\n\
         Terminal=false\n\
         NoDisplay=true\n",
        exe.display()
    );
    if std::fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
        std::fs::create_dir_all(path.parent().context("parent")?)?;
        std::fs::write(&path, content)?;
        println!("[desktop] wrote {}", path.display());
    } else {
        println!("[desktop] up to date: {}", path.display());
    }
    Ok(path)
}

fn remove_file_verbose(p: &Path, what: &str) {
    match std::fs::remove_file(p) {
        Ok(()) => println!("[cleanup] removed {what}: {}", p.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("[cleanup] {what} not present: {}", p.display());
        }
        Err(e) => println!("[cleanup] FAILED to remove {what} {}: {e}", p.display()),
    }
}

// ---------------------------------------------------------------- portal helpers

async fn with_timeout<T>(what: &str, f: impl std::future::Future<Output = T>) -> Result<T> {
    tokio::time::timeout(CALL_TIMEOUT, f)
        .await
        .map_err(|_| anyhow!("{what} timed out after {CALL_TIMEOUT:?} (a dialog may be open?)"))
}

async fn register(conn: &zbus::Connection) -> Result<()> {
    let t = Instant::now();
    let app_id = AppID::try_from(APP_ID).map_err(|e| anyhow!("bad app id: {e}"))?;
    let r = ashpd::register_host_app_with_connection(conn.clone(), app_id).await;
    match r {
        Ok(()) => {
            println!("[registry] Register({APP_ID}) OK in {:.1}ms", ms(t.elapsed().as_nanos() as i128));
            Ok(())
        }
        Err(e) => {
            println!("[registry] Register({APP_ID}) FAILED after {:.1}ms: {e}", ms(t.elapsed().as_nanos() as i128));
            Err(e.into())
        }
    }
}

async fn registry_version_and_reregister(conn: &zbus::Connection) {
    let proxy = zbus::proxy::Builder::<zbus::Proxy<'_>>::new(conn)
        .destination("org.freedesktop.portal.Desktop")
        .and_then(|b| b.path("/org/freedesktop/portal/desktop"))
        .and_then(|b| b.interface("org.freedesktop.host.portal.Registry"))
        .map(|b| b.cache_properties(zbus::proxy::CacheProperties::No));
    let Ok(builder) = proxy else { return };
    let Ok(proxy) = builder.build().await else { return };
    match proxy.get_property::<u32>("version").await {
        Ok(v) => println!("[registry] org.freedesktop.host.portal.Registry version = {v}"),
        Err(e) => println!("[registry] version property unreadable: {e}"),
    }
    // The docs say a second Register on the same connection must fail; if it does,
    // that is positive evidence that the first one stuck to this connection.
    let opts: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
    let r: zbus::Result<()> = proxy.call("Register", &(APP_ID, opts)).await;
    match r {
        Ok(()) => println!("[registry] second Register unexpectedly succeeded"),
        Err(e) => println!("[registry] second Register rejected as documented: {e}"),
    }
}

fn print_shortcuts(label: &str, list: &[ashpd::desktop::global_shortcuts::Shortcut]) {
    if list.is_empty() {
        println!("[{label}] 0 shortcuts");
    }
    for s in list {
        println!(
            "[{label}] id={:?} description={:?} trigger_description={:?}",
            s.id(),
            s.description(),
            s.trigger_description()
        );
    }
}

async fn kga_component_report(conn: &zbus::Connection) {
    let path = format!("/component/{}", APP_ID.replace(['.', '-'], "_"));
    let Ok(kga) = zbus::Proxy::new(conn, KGA_SERVICE, "/kglobalaccel", KGA_IFACE).await else {
        println!("[kglobalaccel] not available (not KDE?)");
        return;
    };
    let comps: Vec<OwnedObjectPath> = match kga.call("allComponents", &()).await {
        Ok(c) => c,
        Err(e) => {
            println!("[kglobalaccel] allComponents failed: {e}");
            return;
        }
    };
    if !comps.iter().any(|c| c.as_str() == path) {
        println!("[kglobalaccel] component {path} not present");
        return;
    }
    let Ok(comp) = zbus::Proxy::new(conn, KGA_SERVICE, path.as_str(), KGA_COMPONENT_IFACE).await
    else {
        return;
    };
    type Info = (String, String, String, String, String, String, Vec<i32>, Vec<i32>);
    let infos: zbus::Result<Vec<Info>> = comp.call("allShortcutInfos", &()).await;
    match infos {
        Ok(infos) => {
            println!("[kglobalaccel] component {path} present, {} shortcut(s)", infos.len());
            for i in infos {
                println!(
                    "[kglobalaccel]   action={:?} friendly={:?} keys={:?} default={:?}",
                    i.0, i.1, i.6, i.7
                );
            }
        }
        Err(e) => println!("[kglobalaccel] component {path} present, allShortcutInfos failed: {e}"),
    }
}

// ---------------------------------------------------------------- probe

async fn probe(o: &Opts) -> Result<()> {
    println!("== probe (non-interactive; BindShortcuts is NOT called) ==");
    println!(
        "[env] XDG_CURRENT_DESKTOP={:?} WAYLAND_DISPLAY={:?} sandboxed={}",
        std::env::var("XDG_CURRENT_DESKTOP").ok(),
        std::env::var("WAYLAND_DISPLAY").ok(),
        Path::new("/.flatpak-info").exists()
    );
    if o.no_desktop {
        remove_file_verbose(&desktop_path()?, ".desktop (for --no-desktop)");
    } else {
        install_desktop()?;
    }
    let conn = zbus::Connection::session().await?;
    println!("[dbus] unique name {:?}", conn.unique_name().map(|n| n.to_string()));
    if o.no_register {
        println!("[registry] skipped (--no-register)");
    } else {
        let r = register(&conn).await;
        if r.is_ok() {
            registry_version_and_reregister(&conn).await;
        }
    }

    let t = Instant::now();
    let gs = with_timeout("GlobalShortcuts proxy", GlobalShortcuts::with_connection(conn.clone())).await??;
    println!(
        "[gs] org.freedesktop.portal.GlobalShortcuts version = {} ({:.1}ms)",
        gs.version(),
        ms(t.elapsed().as_nanos() as i128)
    );

    let t = Instant::now();
    let session = match with_timeout("CreateSession", gs.create_session(Default::default())).await? {
        Ok(s) => s,
        Err(e) => {
            println!("[gs] CreateSession FAILED after {:.1}ms: {e}", ms(t.elapsed().as_nanos() as i128));
            kga_component_report(&conn).await;
            return Ok(());
        }
    };
    println!("[gs] CreateSession OK in {:.1}ms: {session:?}", ms(t.elapsed().as_nanos() as i128));

    let t = Instant::now();
    match with_timeout("ListShortcuts", gs.list_shortcuts(&session, Default::default())).await? {
        Ok(req) => match req.response() {
            Ok(list) => {
                println!("[gs] ListShortcuts OK in {:.1}ms", ms(t.elapsed().as_nanos() as i128));
                print_shortcuts("list", list.shortcuts());
            }
            Err(e) => println!("[gs] ListShortcuts response error: {e}"),
        },
        Err(e) => println!("[gs] ListShortcuts FAILED: {e}"),
    }

    kga_component_report(&conn).await;
    match session.close().await {
        Ok(()) => println!("[gs] Session.Close OK"),
        Err(e) => println!("[gs] Session.Close failed: {e}"),
    }
    println!("== probe done; run --cleanup to remove the .desktop file ==");
    Ok(())
}

// ---------------------------------------------------------------- bind + listen

#[derive(Default)]
struct Stats {
    portal_ts_to_arrival: Vec<f64>,
    kga_to_portal_arrival: Vec<f64>,
    evdev_to_portal_arrival: Vec<f64>,
    kga_to_stamp: Vec<f64>,
    kga_ts_to_stamp: Vec<f64>,
    evdev_to_stamp: Vec<f64>,
}

fn summarize(name: &str, v: &[f64]) {
    if v.is_empty() {
        println!("  {name:<44} n=0");
        return;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let med = s[s.len() / 2];
    println!(
        "  {name:<44} n={:<3} min={:.1}ms median={:.1}ms max={:.1}ms",
        s.len(),
        s[0],
        med,
        s[s.len() - 1]
    );
}

struct KgaPress {
    component: String,
    at: Stamp,
    ts_ms: i64,
}

async fn kga_stream(conn: &zbus::Connection) -> Result<MessageStream> {
    let rule = MatchRule::builder()
        .msg_type(MsgType::Signal)
        .interface(KGA_COMPONENT_IFACE)?
        .member("globalShortcutPressed")?
        .build();
    Ok(MessageStream::for_match_rule(rule, conn, Some(64)).await?)
}

type BoxStream<T> = std::pin::Pin<Box<dyn futures_util::Stream<Item = T> + Send>>;

async fn bind(o: &Opts) -> Result<()> {
    let portal = o.mode == Mode::Bind;
    println!("== {} ==", if portal { "bind + listen" } else { "listen-only (no portal calls)" });
    if portal {
        install_desktop()?;
    }
    let conn = zbus::Connection::session().await?;

    let mut kga = match kga_stream(&conn).await {
        Ok(s) => Some(s),
        Err(e) => {
            println!("[kglobalaccel] cannot subscribe: {e}");
            None
        }
    };
    let _ = std::fs::remove_file(&o.sock);
    let sock = tokio::net::UnixDatagram::bind(&o.sock)
        .with_context(|| format!("bind {}", o.sock.display()))?;
    println!("[sock] listening for --stamp/--evdev-log on {}", o.sock.display());

    let mut gs_session = None;
    let mut activated: BoxStream<ashpd::desktop::global_shortcuts::Activated> =
        Box::pin(futures_util::stream::pending());
    let mut deactivated: BoxStream<ashpd::desktop::global_shortcuts::Deactivated> =
        Box::pin(futures_util::stream::pending());
    let mut changed: BoxStream<ashpd::desktop::global_shortcuts::ShortcutsChanged> =
        Box::pin(futures_util::stream::pending());

    if portal {
        register(&conn).await?;
        let gs = GlobalShortcuts::with_connection(conn.clone()).await?;
        let version = gs.version();
        println!("[gs] version = {version}");
        let t = Instant::now();
        let session = with_timeout("CreateSession", gs.create_session(Default::default())).await??;
        println!("[gs] CreateSession OK in {:.1}ms", ms(t.elapsed().as_nanos() as i128));

        let before =
            with_timeout("ListShortcuts", gs.list_shortcuts(&session, Default::default())).await??;
        print_shortcuts("persisted-before-bind", before.response()?.shortcuts());
        kga_component_report(&conn).await;

        activated = Box::pin(gs.receive_activated().await?);
        deactivated = Box::pin(gs.receive_deactivated().await?);
        changed = Box::pin(gs.receive_shortcuts_changed().await?);

        let shortcut = NewShortcut::new(SHORTCUT_ID, "Translate selection (Quick Translate spike)")
            .preferred_trigger(o.trigger.as_str());
        println!(
            "[gs] BindShortcuts([{SHORTCUT_ID} preferred_trigger={:?}]) — on first bind KDE shows a dialog; accept it",
            o.trigger
        );
        let t = Instant::now();
        let req = gs.bind_shortcuts(&session, &[shortcut], None, Default::default()).await?;
        let bind_ms = ms(t.elapsed().as_nanos() as i128);
        match req.response() {
            Ok(r) => {
                println!("[gs] BindShortcuts OK after {bind_ms:.1}ms (includes any dialog time)");
                print_shortcuts("bound", r.shortcuts());
            }
            Err(e) => println!("[gs] BindShortcuts FAILED/CANCELLED after {bind_ms:.1}ms: {e}"),
        }
        let after = gs.list_shortcuts(&session, Default::default()).await?;
        print_shortcuts("list-after-bind", after.response()?.shortcuts());
        kga_component_report(&conn).await;

        if o.configure {
            if version >= 2 {
                match gs.configure_shortcuts(&session, None, Default::default()).await {
                    Ok(()) => println!("[gs] ConfigureShortcuts OK (system settings should open)"),
                    Err(e) => println!("[gs] ConfigureShortcuts failed: {e}"),
                }
            } else {
                println!("[gs] --configure ignored: version {version} < 2");
            }
        }
        gs_session = Some((gs, session));
    }

    let deadline = if o.listen_secs == 0 {
        None
    } else {
        Some(tokio::time::Instant::now() + Duration::from_secs(o.listen_secs))
    };
    println!(
        "[listen] press the shortcut now; listening {} (Ctrl+C to stop)",
        if o.listen_secs == 0 { "until Ctrl+C".to_owned() } else { format!("{}s", o.listen_secs) }
    );

    let mut stats = Stats::default();
    let mut last_kga: Option<KgaPress> = None;
    let mut last_kga_other: Option<KgaPress> = None;
    let mut last_evdev_real_ns: Option<i128> = None;
    let mut n_act = 0u32;
    let mut buf = [0u8; 512];
    let sleep = async {
        match deadline {
            Some(d) => tokio::time::sleep_until(d).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(sleep);
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            _ = &mut sleep => { println!("[listen] time up"); break; }
            _ = &mut ctrl_c => { println!("[listen] Ctrl+C"); break; }
            Some(a) = activated.next() => {
                let at = now();
                n_act += 1;
                let ts_ms = a.timestamp().as_millis() as i128;
                let token = a.options().get("activation_token")
                    .and_then(|v| <&str>::try_from(&**v).ok().map(str::to_owned));
                let keys: Vec<&String> = a.options().keys().collect();
                println!(
                    "ACTIVATED #{n_act} id={:?} portal_ts={ts_ms}ms options_keys={keys:?} activation_token={}",
                    a.shortcut_id(),
                    match &token { Some(t) => format!("PRESENT(len={}, {:?}…)", t.len(), &t[..t.len().min(12)]), None => "ABSENT".to_owned() }
                );
                println!("  portal ts vs clocks: {}", interpret_ms_timestamp(ts_ms, at));
                let mono_d = ms(at.mono_ns) - ts_ms as f64;
                if (-1.0..5000.0).contains(&mono_d) { stats.portal_ts_to_arrival.push(mono_d); }
                if let Some(k) = &last_kga {
                    let d = ms(at.mono_ns - k.at.mono_ns);
                    if d < 2000.0 {
                        println!("  kglobalaccel signal arrival -> portal Activated arrival: {d:.2}ms (kga ts={}ms, portal ts={ts_ms}ms)", k.ts_ms);
                        stats.kga_to_portal_arrival.push(d);
                    }
                }
                if let Some(ev) = last_evdev_real_ns {
                    let d = ms(at.real_ns - ev);
                    if (0.0..2000.0).contains(&d) {
                        println!("  evdev key-down -> portal Activated arrival: {d:.2}ms");
                        stats.evdev_to_portal_arrival.push(d);
                    }
                }
                if let Some(cmd) = &o.exec {
                    let mut c = tokio::process::Command::new("sh");
                    c.arg("-c").arg(cmd).env("QT_SPIKE_ACTIVATED_MONO_NS", at.mono_ns.to_string());
                    match &token { Some(t) => { c.env("XDG_ACTIVATION_TOKEN", t); } None => { c.env_remove("XDG_ACTIVATION_TOKEN"); } }
                    match c.spawn() {
                        Ok(mut child) => { tokio::spawn(async move { let _ = child.wait().await; }); }
                        Err(e) => println!("  --exec spawn failed: {e}"),
                    }
                }
            }
            Some(d) = deactivated.next() => {
                let at = now();
                println!("DEACTIVATED id={:?} portal_ts={}ms ({})", d.shortcut_id(), d.timestamp().as_millis(), interpret_ms_timestamp(d.timestamp().as_millis() as i128, at));
            }
            Some(c) = changed.next() => {
                println!("SHORTCUTS_CHANGED");
                print_shortcuts("changed", c.shortcuts());
            }
            Some(msg) = async { match kga.as_mut() { Some(s) => s.next().await, None => std::future::pending().await } } => {
                let at = now();
                let Ok(msg) = msg else { continue };
                let Ok((component, shortcut, ts_ms)) = msg.body().deserialize::<(String, String, i64)>() else { continue };
                let ours = component == APP_ID;
                println!(
                    "KGLOBALACCEL pressed component={component:?} shortcut={shortcut:?} ts={ts_ms}ms{} | {}",
                    if ours { " (this spike)" } else { "" },
                    interpret_ms_timestamp(i128::from(ts_ms), at)
                );
                let p = KgaPress { component, at, ts_ms };
                if ours { last_kga = Some(p); } else { last_kga_other = Some(p); }
            }
            r = sock.recv(&mut buf) => {
                let Ok(n) = r else { continue };
                let line = String::from_utf8_lossy(&buf[..n]).to_string();
                let f: Vec<&str> = line.split_whitespace().collect();
                match f.as_slice() {
                    ["evdev", real_ns, code] => {
                        if let Ok(v) = real_ns.parse::<i128>() {
                            last_evdev_real_ns = Some(v);
                            println!("EVDEV key-down code={code} (kernel ts; {:.2}ms before receipt)", ms(now().real_ns - v));
                        }
                    }
                    ["stamp", pid, real_ns, mono_ns, token] => {
                        let (Ok(real), Ok(mono)) = (real_ns.parse::<i128>(), mono_ns.parse::<i128>()) else { continue };
                        println!("STAMP (native shortcut client) pid={pid} activation_token={token}");
                        if let Some(k) = &last_kga_other {
                            let d = ms(mono - k.at.mono_ns);
                            let dts = ms(mono) - k.ts_ms as f64;
                            if (-50.0..5000.0).contains(&d) {
                                println!("  kglobalaccel signal ({}) arrival -> client main(): {d:.2}ms; kga ts -> client main() (if ts is MONOTONIC): {dts:.2}ms", k.component);
                                stats.kga_to_stamp.push(d);
                                if (-1.0..5000.0).contains(&dts) { stats.kga_ts_to_stamp.push(dts); }
                            }
                        }
                        if let Some(ev) = last_evdev_real_ns {
                            let d = ms(real - ev);
                            if (0.0..5000.0).contains(&d) {
                                println!("  evdev key-down -> client main(): {d:.2}ms");
                                stats.evdev_to_stamp.push(d);
                            }
                        }
                    }
                    _ => println!("[sock] unrecognized datagram {line:?}"),
                }
            }
        }
    }

    println!("== summary ==");
    summarize("portal: Activated.timestamp -> arrival (mono)", &stats.portal_ts_to_arrival);
    summarize("portal: kglobalaccel signal -> Activated", &stats.kga_to_portal_arrival);
    summarize("portal: evdev key-down -> Activated", &stats.evdev_to_portal_arrival);
    summarize("native: kglobalaccel signal -> client main()", &stats.kga_to_stamp);
    summarize("native: kga timestamp -> client main()", &stats.kga_ts_to_stamp);
    summarize("native: evdev key-down -> client main()", &stats.evdev_to_stamp);
    if let Some((_gs, session)) = gs_session {
        let _ = session.close().await;
        println!("[gs] session closed.");
    }
    let _ = std::fs::remove_file(&o.sock);
    println!("Run --cleanup when completely done.");
    Ok(())
}

// ---------------------------------------------------------------- stamp

fn stamp(start: Stamp, o: &Opts) -> Result<()> {
    let token = if std::env::var_os("XDG_ACTIVATION_TOKEN").is_some() {
        "PRESENT"
    } else {
        "ABSENT"
    };
    let line = format!("stamp {} {} {} {token}", std::process::id(), start.real_ns, start.mono_ns);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(stamp_log()) {
        let _ = writeln!(f, "{line}");
    }
    let s = StdUnixDatagram::unbound()?;
    match s.send_to(line.as_bytes(), &o.sock) {
        Ok(_) => {}
        Err(e) => eprintln!("stamp: listener not reachable at {}: {e}", o.sock.display()),
    }
    Ok(())
}

// ---------------------------------------------------------------- evdev helper

fn evdev_log(o: &Opts) -> Result<()> {
    use evdev::{EventSummary, KeyCode};
    if o.devices.is_empty() {
        bail!("--evdev-log needs at least one /dev/input/eventN path");
    }
    eprintln!(
        "evdev-log: forwarding ONLY timestamps of non-modifier key-downs pressed while Ctrl/Alt/Meta is held, to {}. Ctrl+C to stop.",
        o.sock.display()
    );
    let mut handles = Vec::new();
    for dev in &o.devices {
        let dev = dev.clone();
        let sock_path = o.sock.clone();
        handles.push(std::thread::spawn(move || -> Result<()> {
            let mut d = evdev::Device::open(&dev).with_context(|| format!("open {}", dev.display()))?;
            let s = StdUnixDatagram::unbound()?;
            let mods = [
                KeyCode::KEY_LEFTCTRL, KeyCode::KEY_RIGHTCTRL, KeyCode::KEY_LEFTALT,
                KeyCode::KEY_RIGHTALT, KeyCode::KEY_LEFTMETA, KeyCode::KEY_RIGHTMETA,
                KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_RIGHTSHIFT,
            ];
            let mut held = 0u32;
            loop {
                for ev in d.fetch_events()? {
                    let ts = ev.timestamp();
                    if let EventSummary::Key(_, code, value) = ev.destructure() {
                        let is_mod = mods.contains(&code);
                        let is_trigger_mod = is_mod && !matches!(code, KeyCode::KEY_LEFTSHIFT | KeyCode::KEY_RIGHTSHIFT);
                        match (is_mod, value) {
                            (true, 1) if is_trigger_mod => held += 1,
                            (true, 0) if is_trigger_mod => held = held.saturating_sub(1),
                            (false, 1) if held > 0 => {
                                let ns = ts.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
                                let _ = s.send_to(format!("evdev {ns} {}", code.0).as_bytes(), &sock_path);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }));
    }
    for h in handles {
        if let Ok(Err(e)) = h.join() {
            eprintln!("evdev-log: {e:#}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- cleanup

async fn cleanup() -> Result<()> {
    println!("== cleanup ==");
    remove_file_verbose(&desktop_path()?, ".desktop");
    remove_file_verbose(&default_sock(), "socket");
    remove_file_verbose(&stamp_log(), "stamp log");
    let conn = zbus::Connection::session().await?;
    let path = format!("/component/{}", APP_ID.replace(['.', '-'], "_"));
    if let Ok(kga) = zbus::Proxy::new(&conn, KGA_SERVICE, "/kglobalaccel", KGA_IFACE).await {
        let comps: Vec<OwnedObjectPath> = kga.call("allComponents", &()).await.unwrap_or_default();
        if comps.iter().any(|c| c.as_str() == path) {
            let comp = zbus::Proxy::new(&conn, KGA_SERVICE, path.as_str(), KGA_COMPONENT_IFACE).await?;
            let names: Vec<String> = comp.call("shortcutNames", &()).await.unwrap_or_default();
            for n in &names {
                let r: zbus::Result<bool> = kga.call("unregister", &(APP_ID, n.as_str())).await;
                println!("[cleanup] kglobalaccel unregister({APP_ID}, {n}) -> {r:?}");
            }
            let r: zbus::Result<bool> = comp.call("cleanUp", &()).await;
            println!("[cleanup] kglobalaccel {path}.cleanUp() -> {r:?}");
        } else {
            println!("[cleanup] kglobalaccel component {path} not present");
        }
        kga_component_report(&conn).await;
    }
    println!(
        "[cleanup] Portal sessions close automatically when the spike process exits.\n\
         [cleanup] If System Settings > Keyboard > Shortcuts still lists \"Quick Translate (portal spike)\",\n\
         [cleanup] select it and remove it. Also remove the native custom shortcut that runs\n\
         [cleanup] `portal-shortcuts-spike --stamp` if you created one (it lives in\n\
         [cleanup] ~/.local/share/applications/net.local.*.desktop)."
    );
    Ok(())
}

// ---------------------------------------------------------------- main

fn main() -> Result<()> {
    // Taken first so `--stamp` measures as close to process start as possible.
    let start = now();
    let o = parse_opts()?;
    match o.mode {
        Mode::Help => {
            print!("{HELP}");
            Ok(())
        }
        Mode::Stamp => stamp(start, &o),
        Mode::EvdevLog => evdev_log(&o),
        Mode::Probe | Mode::Bind | Mode::ListenOnly | Mode::Cleanup => {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
            rt.block_on(async {
                match o.mode {
                    Mode::Probe => probe(&o).await,
                    Mode::Bind | Mode::ListenOnly => bind(&o).await,
                    _ => cleanup().await,
                }
            })
        }
    }
}
