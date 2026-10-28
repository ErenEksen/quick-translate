//! An iced executor that runs on the daemon's Tokio runtime, so iced, zbus and ashpd share one
//! runtime (design D10).

use std::io;
use std::sync::OnceLock;

use tokio::runtime::Handle;

static RUNTIME: OnceLock<Handle> = OnceLock::new();

/// Registers the runtime the shells run on. Call once, before starting a shell.
pub fn install(handle: Handle) {
    if RUNTIME.set(handle).is_err() {
        tracing::debug!("shared runtime already installed");
    }
}

/// Spawns iced tasks and subscriptions on the runtime registered with [`install`].
#[derive(Debug, Clone)]
pub struct SharedExecutor(Handle);

impl iced::Executor for SharedExecutor {
    fn new() -> Result<Self, io::Error> {
        RUNTIME
            .get()
            .cloned()
            .map(Self)
            .ok_or_else(|| io::Error::other("the shared Tokio runtime is not installed"))
    }

    fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        drop(self.0.spawn(future));
    }

    fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        self.0.block_on(future)
    }

    fn enter<R>(&self, f: impl FnOnce() -> R) -> R {
        let _guard = self.0.enter();
        f()
    }
}
