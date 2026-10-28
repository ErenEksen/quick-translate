//! Persistent data-control thread (design D5).
//!
//! The thread owns its own Wayland connection and a data-control device for the first seat. It
//! tracks the latest PRIMARY and CLIPBOARD offers with their MIME types and reads one only when
//! asked. It never creates a data source or sets a selection.

use std::collections::VecDeque;
use std::io;
use std::ops::RangeInclusive;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use quick_translate_core::capabilities::DataControl;
use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::pipe::{PipeFlags, pipe_with};
use tokio::sync::oneshot;
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1 as ext_device, ext_data_control_manager_v1 as ext_manager,
    ext_data_control_offer_v1 as ext_offer,
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1 as wlr_device, zwlr_data_control_manager_v1 as wlr_manager,
    zwlr_data_control_offer_v1 as wlr_offer,
};

use super::{FORBIDDEN_INTERFACES, ProbeError, WaylandDisplay};
use crate::selection::{Payload, pick_text_mime};

/// Upper bound on bytes read from one offer. Far above [`quick_translate_core::MAX_SOURCE_CHARS`]
/// in UTF-8, so only pathological selections are cut short.
pub const MAX_READ_BYTES: usize = 1 << 20;

/// Which selection to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Selection {
    Primary,
    Clipboard,
}

/// A tracked offer. `serial` increases with every new offer on the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferInfo {
    pub serial: u64,
    pub mime_types: Vec<String>,
}

/// What the thread currently tracks. `None` means the selection is empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectionSnapshot {
    pub primary: Option<OfferInfo>,
    pub clipboard: Option<OfferInfo>,
    /// The device or connection is gone; reads fail with [`ReadError::Unavailable`].
    pub finished: bool,
}

/// A global the thread bound on its connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundGlobal {
    pub interface: &'static str,
    pub version: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("{0:?} cannot read PRIMARY (wlr-data-control needs version 2)")]
    Unsupported(DataControl),
    #[error(transparent)]
    Connect(#[from] ProbeError),
    #[error("the compositor does not offer a usable {interface}")]
    MissingGlobal { interface: &'static str },
    #[error("data-control setup failed: {0}")]
    Wayland(String),
    #[error("cannot start the data-control thread: {0}")]
    Thread(#[source] io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("data-control is no longer available")]
    Unavailable,
    #[error("the selection owner did not answer in time")]
    Timeout,
    #[error("reading the selection failed: {0}")]
    Io(#[from] io::Error),
}

/// Handle to the data-control thread. Cheap to clone; the thread stops when the last clone drops.
#[derive(Clone)]
pub struct DataControlHandle {
    inner: Arc<Inner>,
}

struct Inner {
    protocol: DataControl,
    tx: mpsc::Sender<Msg>,
    wake: OwnedFd,
    shared: Arc<Mutex<Shared>>,
}

#[derive(Default)]
struct Shared {
    snapshot: SelectionSnapshot,
    bound: Vec<BoundGlobal>,
}

type Reply = oneshot::Sender<Result<Option<Payload>, ReadError>>;

enum Msg {
    Read {
        which: Selection,
        deadline: Instant,
        reply: Reply,
    },
    Shutdown,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        wake(&self.wake);
    }
}

fn wake(fd: &OwnedFd) {
    let _ = rustix::io::write(fd, &[1]);
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

impl DataControlHandle {
    /// Spawn the thread on the display named by the environment.
    pub fn spawn(protocol: DataControl) -> Result<Self, SpawnError> {
        Self::spawn_on(&WaylandDisplay::from_env()?, protocol)
    }

    /// Spawn the thread on a specific display. Returns once the device is created and the
    /// current selections are known.
    pub fn spawn_on(display: &WaylandDisplay, protocol: DataControl) -> Result<Self, SpawnError> {
        if matches!(protocol, DataControl::Wlr(v) if v < 2) {
            return Err(SpawnError::Unsupported(protocol));
        }
        let conn = display.connect()?;
        let (globals, mut queue) =
            registry_queue_init::<State>(&conn).map_err(|e| SpawnError::Wayland(e.to_string()))?;
        let qh = queue.handle();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let mut state = State {
            primary: None,
            clipboard: None,
            serial: 0,
            finished: false,
            shared: shared.clone(),
        };

        let mut bound = Vec::new();
        let seat: WlSeat = bind(&globals, &qh, 1..=1, &mut bound)?;
        let device = match protocol {
            DataControl::Ext(_) => {
                let manager: ext_manager::ExtDataControlManagerV1 =
                    bind(&globals, &qh, 1..=1, &mut bound)?;
                Device::Ext(manager.get_data_device(&seat, &qh, ()), manager)
            }
            DataControl::Wlr(_) => {
                let manager: wlr_manager::ZwlrDataControlManagerV1 =
                    bind(&globals, &qh, 2..=2, &mut bound)?;
                Device::Wlr(manager.get_data_device(&seat, &qh, ()), manager)
            }
        };
        lock(&shared).bound = bound;
        queue
            .roundtrip(&mut state)
            .map_err(|e| SpawnError::Wayland(e.to_string()))?;
        state.publish();

        let (wake_rx, wake_tx) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)
            .map_err(|e| SpawnError::Thread(e.into()))?;
        let (tx, rx) = mpsc::channel();
        let worker = Worker {
            conn,
            queue,
            state,
            device,
            _seat: seat,
            rx,
            wake_rx,
        };
        thread::Builder::new()
            .name("qt-data-control".into())
            .spawn(move || worker.run())
            .map_err(SpawnError::Thread)?;
        let socket = display.path().display();
        tracing::debug!(?protocol, %socket, "data-control thread started");
        Ok(Self {
            inner: Arc::new(Inner {
                protocol,
                tx,
                wake: wake_tx,
                shared,
            }),
        })
    }

    pub fn protocol(&self) -> DataControl {
        self.inner.protocol
    }

    pub fn snapshot(&self) -> SelectionSnapshot {
        lock(&self.inner.shared).snapshot.clone()
    }

    /// Every global this thread bound. Never contains a [`FORBIDDEN_INTERFACES`] entry.
    pub fn bound_globals(&self) -> Vec<BoundGlobal> {
        lock(&self.inner.shared).bound.clone()
    }

    /// Read the current offer of `which` in the preferred text MIME type.
    ///
    /// Resolves to `Ok(None)` when the selection is empty or offers no text type. The read is
    /// abandoned after `timeout`. Runtime-agnostic: the thread does the I/O.
    pub fn read(
        &self,
        which: Selection,
        timeout: Duration,
    ) -> impl Future<Output = Result<Option<Payload>, ReadError>> + Send + 'static {
        let (reply, rx) = oneshot::channel();
        let sent = self
            .inner
            .tx
            .send(Msg::Read {
                which,
                deadline: Instant::now() + timeout,
                reply,
            })
            .is_ok();
        if sent {
            wake(&self.inner.wake);
        }
        async move {
            if !sent {
                return Err(ReadError::Unavailable);
            }
            rx.await.unwrap_or(Err(ReadError::Unavailable))
        }
    }
}

impl std::fmt::Debug for DataControlHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataControlHandle")
            .field("protocol", &self.inner.protocol)
            .finish_non_exhaustive()
    }
}

/// The only place globals are bound, so the bound set is complete.
fn bind<I>(
    globals: &GlobalList,
    qh: &QueueHandle<State>,
    versions: RangeInclusive<u32>,
    bound: &mut Vec<BoundGlobal>,
) -> Result<I, SpawnError>
where
    I: Proxy + 'static,
    State: Dispatch<I, ()>,
{
    let interface = I::interface().name;
    assert!(
        !FORBIDDEN_INTERFACES.contains(&interface),
        "refusing to bind {interface}"
    );
    let proxy: I = globals
        .bind(qh, versions, ())
        .map_err(|_| SpawnError::MissingGlobal { interface })?;
    bound.push(BoundGlobal {
        interface,
        version: proxy.version(),
    });
    Ok(proxy)
}

enum Device {
    Ext(
        ext_device::ExtDataControlDeviceV1,
        ext_manager::ExtDataControlManagerV1,
    ),
    Wlr(
        wlr_device::ZwlrDataControlDeviceV1,
        wlr_manager::ZwlrDataControlManagerV1,
    ),
}

impl Device {
    fn destroy(&self) {
        match self {
            Self::Ext(d, m) => {
                d.destroy();
                m.destroy();
            }
            Self::Wlr(d, m) => {
                d.destroy();
                m.destroy();
            }
        }
    }
}

#[derive(Clone)]
enum Offer {
    Ext(ext_offer::ExtDataControlOfferV1),
    Wlr(wlr_offer::ZwlrDataControlOfferV1),
}

/// User data of an offer: the MIME types announced for it.
type OfferMimes = Mutex<Vec<String>>;

impl Offer {
    fn mime_types(&self) -> Vec<String> {
        let data = match self {
            Self::Ext(o) => o.data::<OfferMimes>(),
            Self::Wlr(o) => o.data::<OfferMimes>(),
        };
        data.map(|m| m.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .unwrap_or_default()
    }

    fn receive(&self, mime: &str, fd: &OwnedFd) {
        match self {
            Self::Ext(o) => o.receive(mime.to_owned(), fd.as_fd()),
            Self::Wlr(o) => o.receive(mime.to_owned(), fd.as_fd()),
        }
    }

    fn destroy(&self) {
        match self {
            Self::Ext(o) => o.destroy(),
            Self::Wlr(o) => o.destroy(),
        }
    }

    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ext(a), Self::Ext(b)) => a == b,
            (Self::Wlr(a), Self::Wlr(b)) => a == b,
            _ => false,
        }
    }
}

struct Tracked {
    offer: Offer,
    serial: u64,
}

struct State {
    primary: Option<Tracked>,
    clipboard: Option<Tracked>,
    serial: u64,
    finished: bool,
    shared: Arc<Mutex<Shared>>,
}

impl State {
    fn slot(&mut self, which: Selection) -> &mut Option<Tracked> {
        match which {
            Selection::Primary => &mut self.primary,
            Selection::Clipboard => &mut self.clipboard,
        }
    }

    fn set_selection(&mut self, which: Selection, offer: Option<Offer>) {
        self.serial += 1;
        let serial = self.serial;
        let new = offer.map(|offer| Tracked { offer, serial });
        if let Some(t) = &new {
            tracing::debug!(?which, mime_types = ?t.offer.mime_types(), "selection offer");
        } else {
            tracing::debug!(?which, "selection cleared");
        }
        let old = std::mem::replace(self.slot(which), new);
        if let Some(old) = old {
            let other = match which {
                Selection::Primary => &self.clipboard,
                Selection::Clipboard => &self.primary,
            };
            let shared = other.as_ref().is_some_and(|t| t.offer.same(&old.offer));
            let reused = self
                .slot(which)
                .as_ref()
                .is_some_and(|t| t.offer.same(&old.offer));
            if !shared && !reused {
                old.offer.destroy();
            }
        }
        self.publish();
    }

    fn finish(&mut self) {
        tracing::debug!("data-control device finished");
        self.finished = true;
        for which in [Selection::Primary, Selection::Clipboard] {
            if let Some(t) = self.slot(which).take() {
                t.offer.destroy();
            }
        }
        self.publish();
    }

    fn publish(&self) {
        let info = |t: &Option<Tracked>| {
            t.as_ref().map(|t| OfferInfo {
                serial: t.serial,
                mime_types: t.offer.mime_types(),
            })
        };
        lock(&self.shared).snapshot = SelectionSnapshot {
            primary: info(&self.primary),
            clipboard: info(&self.clipboard),
            finished: self.finished,
        };
    }
}

struct Worker {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    device: Device,
    _seat: WlSeat,
    rx: mpsc::Receiver<Msg>,
    wake_rx: OwnedFd,
}

impl Worker {
    fn run(mut self) {
        if let Err(e) = self.event_loop() {
            tracing::warn!("data-control thread stopped: {e}");
            self.state.finish();
        }
        self.device.destroy();
        let _ = self.conn.flush();
        // Answer anything still queued so no caller waits on a dropped thread.
        while let Ok(msg) = self.rx.try_recv() {
            if let Msg::Read { reply, .. } = msg {
                let _ = reply.send(Err(ReadError::Unavailable));
            }
        }
    }

    fn event_loop(&mut self) -> Result<(), String> {
        loop {
            self.queue
                .dispatch_pending(&mut self.state)
                .map_err(|e| e.to_string())?;
            self.queue.flush().map_err(|e| e.to_string())?;
            let Some(guard) = self.queue.prepare_read() else {
                continue;
            };
            let (wayland_ready, wake_ready) = {
                let conn_fd = guard.connection_fd();
                let mut fds = [
                    PollFd::new(&conn_fd, PollFlags::IN),
                    PollFd::new(&self.wake_rx, PollFlags::IN),
                ];
                match rustix::event::poll(&mut fds, None) {
                    Ok(_) => {}
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(e) => return Err(e.to_string()),
                }
                (!fds[0].revents().is_empty(), !fds[1].revents().is_empty())
            };
            if wayland_ready {
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(e))
                        if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.to_string()),
                }
                self.queue
                    .dispatch_pending(&mut self.state)
                    .map_err(|e| e.to_string())?;
            } else {
                drop(guard);
            }
            if wake_ready {
                let mut buf = [0u8; 64];
                while matches!(rustix::io::read(&self.wake_rx, &mut buf), Ok(n) if n > 0) {}
                if !self.handle_messages() {
                    return Ok(());
                }
            }
        }
    }

    /// Returns `false` on shutdown.
    fn handle_messages(&mut self) -> bool {
        let mut pending: VecDeque<(Selection, Instant, Reply)> = VecDeque::new();
        loop {
            match self.rx.try_recv() {
                Ok(Msg::Shutdown) | Err(TryRecvError::Disconnected) => {
                    for (_, _, reply) in pending {
                        let _ = reply.send(Err(ReadError::Unavailable));
                    }
                    return false;
                }
                Ok(Msg::Read {
                    which,
                    deadline,
                    reply,
                }) => pending.push_back((which, deadline, reply)),
                Err(TryRecvError::Empty) => break,
            }
        }
        for (which, deadline, reply) in pending {
            let result = self.read_selection(which, deadline);
            let _ = reply.send(result);
        }
        true
    }

    fn read_selection(
        &mut self,
        which: Selection,
        deadline: Instant,
    ) -> Result<Option<Payload>, ReadError> {
        if self.state.finished {
            return Err(ReadError::Unavailable);
        }
        let Some(tracked) = self.state.slot(which).as_ref() else {
            tracing::debug!(?which, "selection empty");
            return Ok(None);
        };
        let mime_types = tracked.offer.mime_types();
        let Some(mime) = pick_text_mime(&mime_types) else {
            tracing::debug!(?which, ?mime_types, "selection has no text type");
            return Ok(None);
        };
        let mime = mime.to_owned();
        let (read_end, write_end) = pipe_with(PipeFlags::CLOEXEC).map_err(io::Error::from)?;
        tracked.offer.receive(&mime, &write_end);
        self.conn
            .flush()
            .map_err(|e| io::Error::other(e.to_string()))?;
        drop(write_end);
        let (bytes, capped) = read_to_end(&read_end, deadline)?;
        tracing::debug!(?which, %mime, bytes = bytes.len(), capped, "selection read");
        Ok(Some(Payload {
            mime: Some(mime),
            bytes,
            capped,
        }))
    }
}

/// Read a pipe to EOF, giving up at `deadline`. Never blocks past it.
fn read_to_end(fd: &OwnedFd, deadline: Instant) -> Result<(Vec<u8>, bool), ReadError> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(ReadError::Timeout);
        }
        let timeout = Timespec::try_from(left).map_err(|_| ReadError::Timeout)?;
        let mut fds = [PollFd::new(fd, PollFlags::IN)];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Err(ReadError::Timeout),
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(io::Error::from(e).into()),
        }
        match rustix::io::read(fd, &mut buf[..]) {
            Ok(0) => return Ok((out, false)),
            Ok(n) => {
                let room = MAX_READ_BYTES - out.len();
                out.extend_from_slice(&buf[..n.min(room)]);
                if n >= room {
                    return Ok((out, true));
                }
            }
            Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => {}
            Err(e) => return Err(io::Error::from(e).into()),
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

macro_rules! impl_dispatch {
    ($variant:ident, $manager:ty, $mgr_mod:ident, $device:ty, $dev_mod:ident, $offer:ty, $offer_mod:ident) => {
        impl Dispatch<$manager, ()> for State {
            fn event(
                _: &mut Self,
                _: &$manager,
                _: $mgr_mod::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }

        impl Dispatch<$device, ()> for State {
            fn event(
                state: &mut Self,
                _: &$device,
                event: $dev_mod::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                match event {
                    $dev_mod::Event::DataOffer { .. } => {}
                    $dev_mod::Event::Selection { id } => {
                        state.set_selection(Selection::Clipboard, id.map(Offer::$variant))
                    }
                    $dev_mod::Event::PrimarySelection { id } => {
                        state.set_selection(Selection::Primary, id.map(Offer::$variant))
                    }
                    $dev_mod::Event::Finished => state.finish(),
                    _ => {}
                }
            }

            event_created_child!(State, $device, [
                $dev_mod::EVT_DATA_OFFER_OPCODE => ($offer, OfferMimes::default()),
            ]);
        }

        impl Dispatch<$offer, OfferMimes> for State {
            fn event(
                _: &mut Self,
                _: &$offer,
                event: $offer_mod::Event,
                mimes: &OfferMimes,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let $offer_mod::Event::Offer { mime_type } = event {
                    mimes.lock().unwrap_or_else(|e| e.into_inner()).push(mime_type);
                }
            }
        }
    };
}

impl_dispatch!(
    Ext,
    ext_manager::ExtDataControlManagerV1,
    ext_manager,
    ext_device::ExtDataControlDeviceV1,
    ext_device,
    ext_offer::ExtDataControlOfferV1,
    ext_offer
);
impl_dispatch!(
    Wlr,
    wlr_manager::ZwlrDataControlManagerV1,
    wlr_manager,
    wlr_device::ZwlrDataControlDeviceV1,
    wlr_device,
    wlr_offer::ZwlrDataControlOfferV1,
    wlr_offer
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wlr_v1_is_unsupported() {
        let display = WaylandDisplay::new("/nonexistent", "wayland-0");
        assert!(matches!(
            DataControlHandle::spawn_on(&display, DataControl::Wlr(1)),
            Err(SpawnError::Unsupported(DataControl::Wlr(1)))
        ));
        assert!(matches!(
            DataControlHandle::spawn_on(&display, DataControl::Wlr(2)),
            Err(SpawnError::Connect(_))
        ));
    }

    #[test]
    fn read_to_end_until_eof() {
        let (r, w) = pipe_with(PipeFlags::CLOEXEC).unwrap();
        rustix::io::write(&w, b"hello").unwrap();
        drop(w);
        let (bytes, capped) = read_to_end(&r, Instant::now() + Duration::from_secs(1)).unwrap();
        assert_eq!((bytes.as_slice(), capped), (&b"hello"[..], false));
    }

    #[test]
    fn read_to_end_times_out_when_owner_is_silent() {
        let (r, _w) = pipe_with(PipeFlags::CLOEXEC).unwrap();
        let start = Instant::now();
        let err = read_to_end(&r, start + Duration::from_millis(50)).unwrap_err();
        assert!(matches!(err, ReadError::Timeout));
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn read_to_end_caps_huge_offers() {
        let (r, w) = pipe_with(PipeFlags::CLOEXEC).unwrap();
        let writer = thread::spawn(move || {
            let chunk = vec![b'a'; 64 * 1024];
            while rustix::io::write(&w, &chunk).is_ok() {}
        });
        let (bytes, capped) = read_to_end(&r, Instant::now() + Duration::from_secs(5)).unwrap();
        assert_eq!(bytes.len(), MAX_READ_BYTES);
        assert!(capped);
        drop(r);
        writer.join().unwrap();
    }
}
