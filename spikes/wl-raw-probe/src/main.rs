//! Throwaway spike for tasks.md 2.1 (protocol level, no toolkit).
//!
//! Creates one transparent layer surface per output (layer overlay, anchored on
//! all four edges, exclusive zone -1, keyboard interactivity exclusive) and logs,
//! in ms since the layer surfaces were created and committed:
//! configure, buffer commit, first frame callback, wl_surface.enter(output),
//! keyboard enter, pointer enter (surface-local position + output), first motion.
//!
//! The buffer is a 1x1 transparent wl_shm buffer stretched with wp_viewporter
//! (this is also exactly the probe surface of fallback B). `--full-buffer`
//! attaches a full-size transparent buffer instead (closer to what a GPU
//! toolkit does).
//!
//! SAFETY NET: the process always exits after `--ms` (default 1500, max 3000).

use std::io::Write as _;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
    wl_keyboard::WlKeyboard, wl_output::WlOutput, wl_pointer::WlPointer, wl_registry::WlRegistry,
    wl_seat::WlSeat, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

#[derive(Default, Clone)]
struct OutputInfo {
    name: String,
    scale: i32,
    mode: (i32, i32),
}

struct Unit {
    output_idx: usize,
    surface: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    viewport: Option<WpViewport>,
    configured: bool,
    framed: bool,
}

struct State {
    t0: Instant,
    compositor: WlCompositor,
    shm: WlShm,
    layer_shell: ZwlrLayerShellV1,
    viewporter: Option<WpViewporter>,
    _seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    outputs: Vec<(WlOutput, OutputInfo)>,
    units: Vec<Unit>,
    full_buffer: bool,
    destroy_losers: bool,
    kb_late: bool,
    winner: Option<usize>,
    first_motion_logged: bool,
    pointer_surface: Option<usize>,
    buffers: Vec<WlBuffer>,
    keep: Vec<std::fs::File>,
}

impl State {
    fn ms(&self) -> f64 {
        self.t0.elapsed().as_secs_f64() * 1000.0
    }
    fn log(&self, msg: impl AsRef<str>) {
        println!("[{:8.2} ms] {}", self.ms(), msg.as_ref());
        let _ = std::io::stdout().flush();
    }
    fn unit_of(&self, surface: &WlSurface) -> Option<usize> {
        self.units.iter().position(|u| &u.surface == surface)
    }
    fn out_name(&self, unit: usize) -> String {
        self.outputs
            .get(self.units[unit].output_idx)
            .map(|(_, i)| i.name.clone())
            .unwrap_or_default()
    }

    fn make_buffer(&mut self, w: i32, h: i32, qh: &QueueHandle<Self>) -> WlBuffer {
        let stride = w * 4;
        let size = (stride * h) as u64;
        let fd = rustix::fs::memfd_create("qt-probe", rustix::fs::MemfdFlags::CLOEXEC)
            .expect("memfd_create");
        rustix::fs::ftruncate(&fd, size).expect("ftruncate");
        // Zero-filled memory == fully transparent ARGB8888 (premultiplied).
        let file = std::fs::File::from(fd);
        let pool: WlShmPool = self.shm.create_pool(file.as_fd(), size as i32, qh, ());
        let buf = pool.create_buffer(
            0,
            w,
            h,
            stride,
            wayland_client::protocol::wl_shm::Format::Argb8888,
            qh,
            (),
        );
        pool.destroy();
        self.keep.push(file);
        self.buffers.push(buf.clone());
        buf
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let life_ms: u64 = get("--ms").and_then(|s| s.parse().ok()).unwrap_or(1500).min(3000);
    let full_buffer = args.iter().any(|a| a == "--full-buffer");
    let layer = match get("--layer").as_deref() {
        Some("top") => Layer::Top,
        _ => Layer::Overlay,
    };
    let kb_late = args.iter().any(|a| a == "--kb-late");
    let kb = match get("--kb").as_deref() {
        _ if kb_late => KeyboardInteractivity::None,
        Some("ondemand") => KeyboardInteractivity::OnDemand,
        Some("none") => KeyboardInteractivity::None,
        _ => KeyboardInteractivity::Exclusive,
    };

    // Hard safety net: never keep the keyboard grabbed longer than life_ms.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(life_ms));
        println!("[safety] {life_ms} ms elapsed, exiting");
        let _ = std::io::stdout().flush();
        std::process::exit(0);
    });

    let conn = Connection::connect_to_env().expect("no wayland connection");
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let layer_shell: ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ()).expect("zwlr_layer_shell_v1");
    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    let seat: Option<WlSeat> = globals.bind(&qh, 1..=7, ()).ok();

    let mut state = State {
        t0: Instant::now(),
        compositor,
        shm,
        layer_shell,
        viewporter,
        _seat: seat,
        pointer: None,
        keyboard: None,
        outputs: Vec::new(),
        units: Vec::new(),
        full_buffer,
        destroy_losers: args.iter().any(|a| a == "--destroy-losers"),
        kb_late: args.iter().any(|a| a == "--kb-late"),
        winner: None,
        first_motion_logged: false,
        pointer_surface: None,
        buffers: Vec::new(),
        keep: Vec::new(),
    };

    let output_globals: Vec<(u32, u32)> = globals.contents().with_list(|list| {
        list.iter()
            .filter(|g| g.interface == "wl_output")
            .map(|g| (g.name, g.version))
            .collect()
    });
    for (name, version) in output_globals {
        let o: WlOutput = globals
            .registry()
            .bind(name, version.min(4), &qh, state.outputs.len());
        state.outputs.push((o, OutputInfo { scale: 1, ..Default::default() }));
    }
    // Two roundtrips: output info + seat capabilities (pointer/keyboard objects).
    queue.roundtrip(&mut state).expect("roundtrip");
    queue.roundtrip(&mut state).expect("roundtrip");

    println!(
        "env: WAYLAND_DISPLAY={:?} XDG_CURRENT_DESKTOP={:?} layer_shell_v={} viewporter={} full_buffer={} layer={:?} kb={:?}",
        std::env::var("WAYLAND_DISPLAY").ok(),
        std::env::var("XDG_CURRENT_DESKTOP").ok(),
        state.layer_shell.version(),
        state.viewporter.is_some(),
        state.full_buffer,
        layer,
        kb
    );
    for (_, info) in &state.outputs {
        println!("output: name={} mode={:?} scale={}", info.name, info.mode, info.scale);
    }

    // T0: right before creating the surfaces.
    state.t0 = Instant::now();
    for idx in 0..state.outputs.len() {
        let output = state.outputs[idx].0.clone();
        let surface = state.compositor.create_surface(&qh, ());
        let layer_surface =
            state
                .layer_shell
                .get_layer_surface(&surface, Some(&output), layer, "qt-probe".into(), &qh, ());
        layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(kb);
        let viewport = state
            .viewporter
            .as_ref()
            .filter(|_| !full_buffer)
            .map(|v| v.get_viewport(&surface, &qh, ()));
        surface.commit();
        state.units.push(Unit {
            output_idx: idx,
            surface,
            layer: layer_surface,
            viewport,
            configured: false,
            framed: false,
        });
        state.log(format!("created layer surface #{idx} on output {}", state.outputs[idx].1.name));
    }

    loop {
        if let Err(e) = queue.blocking_dispatch(&mut state) {
            state.log(format!("dispatch error: {e}"));
            std::process::exit(1);
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlOutput, usize> for State {
    fn event(state: &mut Self, _: &WlOutput, event: <WlOutput as Proxy>::Event, idx: &usize, _: &Connection, _: &QueueHandle<Self>) {
        use wayland_client::protocol::wl_output::Event;
        let info = &mut state.outputs[*idx].1;
        match event {
            Event::Name { name } => info.name = name,
            Event::Scale { factor } => info.scale = factor,
            Event::Mode { width, height, .. } => info.mode = (width, height),
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(state: &mut Self, seat: &WlSeat, event: <WlSeat as Proxy>::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        use wayland_client::protocol::wl_seat::{Capability, Event};
        if let Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
            if caps.contains(Capability::Keyboard) && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(state: &mut Self, layer: &ZwlrLayerSurfaceV1, event: <ZwlrLayerSurfaceV1 as Proxy>::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        let Some(i) = state.units.iter().position(|u| &u.layer == layer) else { return };
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                let first = !state.units[i].configured;
                state.units[i].configured = true;
                state.log(format!("configure #{i} ({}) {width}x{height} first={first}", state.out_name(i)));
                if first {
                    let (bw, bh) = if state.units[i].viewport.is_some() {
                        (1, 1)
                    } else {
                        (width as i32, height as i32)
                    };
                    let buf = state.make_buffer(bw, bh, qh);
                    let surface = state.units[i].surface.clone();
                    if let Some(vp) = &state.units[i].viewport {
                        vp.set_destination(width as i32, height as i32);
                    }
                    surface.attach(Some(&buf), 0, 0);
                    surface.damage_buffer(0, 0, bw, bh);
                    surface.frame(qh, i);
                    surface.commit();
                    state.log(format!("committed buffer #{i} ({bw}x{bh})"));
                }
            }
            zwlr_layer_surface_v1::Event::Closed => state.log(format!("closed #{i}")),
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, usize> for State {
    fn event(state: &mut Self, _: &WlCallback, _: <WlCallback as Proxy>::Event, i: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if !state.units[*i].framed {
            state.units[*i].framed = true;
            state.log(format!("first frame callback done #{i}"));
        }
    }
}

impl Dispatch<WlSurface, ()> for State {
    fn event(state: &mut Self, surface: &WlSurface, event: <WlSurface as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use wayland_client::protocol::wl_surface::Event;
        if let Event::Enter { output } = event {
            let name = state
                .outputs
                .iter()
                .find(|(o, _)| *o == output)
                .map(|(_, i)| i.name.clone())
                .unwrap_or_default();
            state.log(format!("wl_surface.enter #{:?} output={name}", state.unit_of(surface)));
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(state: &mut Self, _: &WlKeyboard, event: <WlKeyboard as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use wayland_client::protocol::wl_keyboard::Event;
        match event {
            Event::Enter { surface, .. } => {
                let u = state.unit_of(&surface);
                let name = u.map(|u| state.out_name(u)).unwrap_or_default();
                state.log(format!("keyboard enter #{u:?} ({name})"));
            }
            Event::Leave { surface, .. } => state.log(format!("keyboard leave #{:?}", state.unit_of(&surface))),
            Event::Key { key, state: ks, .. } => state.log(format!("key {key} {ks:?}")),
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(state: &mut Self, _: &WlPointer, event: <WlPointer as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use wayland_client::protocol::wl_pointer::Event;
        match event {
            Event::Enter { surface, surface_x, surface_y, .. } => {
                let u = state.unit_of(&surface);
                state.pointer_surface = u;
                let name = u.map(|u| state.out_name(u)).unwrap_or_default();
                state.log(format!(
                    "pointer enter #{u:?} output={name} pos=({surface_x:.1}, {surface_y:.1}) [no motion before it: {}]",
                    !state.first_motion_logged
                ));
                if let (Some(w), None) = (u, state.winner) {
                    state.winner = Some(w);
                    if state.kb_late {
                        state.units[w].layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
                        state.units[w].surface.commit();
                        state.log(format!("winner #{w}: set keyboard_interactivity=exclusive + commit"));
                    }
                    if state.destroy_losers {
                        for (j, unit) in state.units.iter().enumerate() {
                            if j != w {
                                unit.layer.destroy();
                                unit.surface.destroy();
                            }
                        }
                        state.log(format!("winner #{w}: destroyed {} loser surface(s)", state.units.len() - 1));
                    }
                }
            }
            Event::Leave { surface, .. } => state.log(format!("pointer leave #{:?}", state.unit_of(&surface))),
            Event::Motion { surface_x, surface_y, .. } => {
                if !state.first_motion_logged {
                    state.first_motion_logged = true;
                    state.log(format!(
                        "first pointer motion #{:?} pos=({surface_x:.1}, {surface_y:.1})",
                        state.pointer_surface
                    ));
                }
            }
            Event::Button { button, state: bs, .. } => state.log(format!("button {button} {bs:?}")),
            _ => {}
        }
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ignore ZwlrLayerShellV1);
delegate_noop!(State: ignore WpViewporter);
delegate_noop!(State: ignore WpViewport);

