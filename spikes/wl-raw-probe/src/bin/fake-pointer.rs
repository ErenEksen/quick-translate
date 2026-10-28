//! Test-harness helper for *nested / headless* test compositors only
//! (`kwin_wayland --virtual`, headless sway/Hyprland/niri in a container):
//! creates a fake pointer device via org_kde_kwin_fake_input (KWin) or
//! zwlr_virtual_pointer_v1 (wlroots & co.; absolute coordinates are mapped over
//! the extent given by env `QT_EXTENT=W,H`, default 1920,1080), warps it to an
//! absolute position, then stays idle (no further motion) for N seconds so the
//! seat keeps its pointer capability. Never point this at a live session.
//!
//! usage: fake-pointer X Y SECONDS [SCRIPT]
//! SCRIPT: `;`-separated steps `MS:move:X,Y` `MS:key:EVDEV_CODE` or `MS:click` (MS since start).

use std::time::Duration;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

enum Dev {
    Kde(OrgKdeKwinFakeInput),
    Wlr(ZwlrVirtualPointerV1, u32, u32),
}
impl Dev {
    fn ts() -> u32 {
        (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() & 0xffff_ffff) as u32
    }
    fn abs(&self, x: f64, y: f64) {
        match self {
            Dev::Kde(fi) => fi.pointer_motion_absolute(x, y),
            Dev::Wlr(vp, w, h) => {
                vp.motion_absolute(Self::ts(), x as u32, y as u32, *w, *h);
                vp.frame();
            }
        }
    }
    fn click(&self) {
        match self {
            Dev::Kde(fi) => {
                fi.button(0x110, 1);
                fi.button(0x110, 0);
            }
            Dev::Wlr(vp, _, _) => {
                use wayland_client::protocol::wl_pointer::ButtonState;
                vp.button(Self::ts(), 0x110, ButtonState::Pressed);
                vp.frame();
                vp.button(Self::ts(), 0x110, ButtonState::Released);
                vp.frame();
            }
        }
    }
    fn key(&self, code: u32) {
        if let Dev::Kde(fi) = self {
            fi.keyboard_key(code, 1);
            fi.keyboard_key(code, 0);
        } else {
            println!("key injection not supported on wlr virtual pointer backend");
        }
    }
}

struct S;
impl Dispatch<WlRegistry, GlobalListContents> for S {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
delegate_noop!(S: ignore OrgKdeKwinFakeInput);
delegate_noop!(S: ignore ZwlrVirtualPointerManagerV1);
delegate_noop!(S: ignore ZwlrVirtualPointerV1);
delegate_noop!(S: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(S: ignore ZwpVirtualKeyboardV1);
delegate_noop!(S: ignore wayland_client::protocol::wl_seat::WlSeat);

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<f64> = argv.iter().take(3).filter_map(|s| s.parse().ok()).collect();
    let script = argv.get(3).cloned().unwrap_or_default();
    let (x, y, secs) = (a.first().copied().unwrap_or(500.0), a.get(1).copied().unwrap_or(400.0), a.get(2).copied().unwrap_or(10.0));
    assert!(
        std::env::var("WAYLAND_DISPLAY").is_ok_and(|d| d.starts_with("qt-")),
        "refusing: WAYLAND_DISPLAY must be a nested test socket (qt-*)"
    );
    let conn = Connection::connect_to_env().unwrap();
    let (globals, mut q) = registry_queue_init::<S>(&conn).unwrap();
    let qh = q.handle();
    let dev = if let Ok(fi) = globals.bind::<OrgKdeKwinFakeInput, _, _>(&qh, 3..=5, ()) {
        fi.authenticate("qt-probe".into(), "spike test harness".into());
        Dev::Kde(fi)
    } else {
        let m: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).expect("no fake-input or virtual-pointer global");
        let ext: Vec<u32> = std::env::var("QT_EXTENT").unwrap_or("1920,1080".into()).split(',').map(|v| v.parse().unwrap()).collect();
        Dev::Wlr(m.create_virtual_pointer(None, &qh, ()), ext[0], ext[1])
    };
    // Optional wlroots virtual keyboard (gives the seat a keyboard capability).
    // QT_KEYMAP=<xkb keymap text file> (e.g. from `xkbcli-compile-keymap`).
    let vk: Option<ZwpVirtualKeyboardV1> = match (std::env::var("QT_KEYMAP"), &dev) {
        (Ok(path), Dev::Wlr(..)) => {
            let mgr: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).expect("virtual keyboard manager");
            let seat: wayland_client::protocol::wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).expect("seat");
            let kb = mgr.create_virtual_keyboard(&seat, &qh, ());
            let mut data = std::fs::read(&path).expect("keymap");
            data.push(0);
            let f = std::fs::File::from(rustix::fs::memfd_create("keymap", rustix::fs::MemfdFlags::CLOEXEC).unwrap());
            use std::io::Write;
            (&f).write_all(&data).unwrap();
            use std::os::fd::AsFd;
            kb.keymap(1, f.as_fd(), data.len() as u32);
            Some(kb)
        }
        _ => None,
    };
    q.roundtrip(&mut S).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    dev.abs(x, y);
    q.roundtrip(&mut S).unwrap();
    println!("fake pointer at ({x}, {y}); idling {secs}s");
    let start = std::time::Instant::now();
    for step in script.split(';').filter(|s| !s.is_empty()) {
        let parts: Vec<&str> = step.split(':').collect();
        let at: u64 = parts[0].parse().unwrap();
        let now = start.elapsed().as_millis() as u64;
        if at > now {
            std::thread::sleep(Duration::from_millis(at - now));
        }
        match parts.get(1).copied() {
            Some("move") => {
                let xy: Vec<f64> = parts[2].split(',').map(|v| v.parse().unwrap()).collect();
                dev.abs(xy[0], xy[1]);
            }
            Some("key") => {
                let code: u32 = parts[2].parse().unwrap();
                if let Some(kb) = &vk {
                    kb.key(Dev::ts(), code, 1);
                    kb.key(Dev::ts(), code, 0);
                } else {
                    dev.key(code);
                }
            }
            Some("click") => {
                dev.click();
            }
            _ => {}
        }
        q.roundtrip(&mut S).unwrap();
        println!("[fake-pointer {} ms] {step}", start.elapsed().as_millis());
    }
    std::thread::sleep(Duration::from_secs_f64(secs).saturating_sub(start.elapsed()));
}
