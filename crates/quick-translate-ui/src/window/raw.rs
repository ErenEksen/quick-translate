//! The only place that touches raw Wayland pointers: plugging the toolkit clipboard and the
//! xdg-activation client into the `wl_display` winit owns.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::XdgActivationV1;

/// A `wl_display` that outlives every user of this value.
///
/// Built either from a [`Connection`] (the holder keeps a clone of it) or from a window of the
/// window shell, whose display belongs to winit's event loop and lives as long as the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DisplayPtr(NonNull<c_void>);

// SAFETY: libwayland's `wl_display` is thread-safe; the pointer is only handed to libwayland.
unsafe impl Send for DisplayPtr {}
// SAFETY: as above, shared access goes through libwayland's own locking.
unsafe impl Sync for DisplayPtr {}

impl DisplayPtr {
    pub(crate) fn from_connection(connection: &Connection) -> Option<Self> {
        NonNull::new(connection.backend().display_ptr().cast()).map(Self)
    }

    /// The display of an iced window, `None` when it is not a Wayland window.
    pub(crate) fn from_window(window: &dyn iced::window::Window) -> Option<Self> {
        match window.display_handle().ok()?.as_raw() {
            RawDisplayHandle::Wayland(handle) => Some(Self(handle.display)),
            _ => None,
        }
    }

    pub(crate) fn addr(self) -> usize {
        self.0.as_ptr() as usize
    }

    /// A smithay-clipboard instance with its own worker thread and event queue.
    pub(crate) fn clipboard(self) -> smithay_clipboard::Clipboard {
        // SAFETY: `DisplayPtr` is only built from displays that outlive their users (see above).
        unsafe { smithay_clipboard::Clipboard::new(self.0.as_ptr()) }
    }

    /// A guest connection on the display: never closes it.
    fn connection(self) -> Connection {
        // SAFETY: as for `clipboard`.
        let backend = unsafe { Backend::from_foreign_display(self.0.as_ptr().cast()) };
        Connection::from_backend(backend)
    }
}

/// `xdg_activation_v1` bound on a foreign display, on its own event queue.
pub(crate) struct Activator {
    connection: Connection,
    global: XdgActivationV1,
    _queue: Mutex<EventQueue<Globals>>,
}

impl std::fmt::Debug for Activator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Activator").finish_non_exhaustive()
    }
}

struct Globals;

impl Dispatch<WlRegistry, GlobalListContents> for Globals {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Globals: ignore XdgActivationV1);

impl Activator {
    /// Binds the global. Blocks for two roundtrips, so call it off the UI thread. `None` when the
    /// compositor has no xdg-activation.
    pub(crate) fn bind(display: DisplayPtr) -> Option<Self> {
        let connection = display.connection();
        let (globals, mut queue) = registry_queue_init::<Globals>(&connection).ok()?;
        let global = globals
            .bind::<XdgActivationV1, _, _>(&queue.handle(), 1..=1, ())
            .ok()?;
        // The clipboard instances started with this bind need about two roundtrips to create
        // their data devices; activating before that would focus the popup first, and some
        // compositors only send the selection to a data device on focus changes.
        queue.roundtrip(&mut Globals).ok()?;
        Some(Self {
            connection,
            global,
            _queue: Mutex::new(queue),
        })
    }

    /// Asks the compositor to activate `window` with `token`. Never blocks.
    pub(crate) fn activate(&self, token: &str, window: &dyn iced::window::Window) -> bool {
        let Some(RawWindowHandle::Wayland(handle)) =
            window.window_handle().ok().map(|handle| handle.as_raw())
        else {
            return false;
        };
        // SAFETY: the surface belongs to `window`, which is borrowed for this whole call, and the
        // proxy built from it does not escape the call.
        let id =
            unsafe { ObjectId::from_ptr(WlSurface::interface(), handle.surface.as_ptr().cast()) };
        let Some(surface) = id
            .ok()
            .and_then(|id| WlSurface::from_id(&self.connection, id).ok())
        else {
            return false;
        };
        self.global.activate(token.to_owned(), &surface);
        self.connection.flush().is_ok()
    }
}
