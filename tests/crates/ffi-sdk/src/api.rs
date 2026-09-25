use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    task::Poll,
};

use futures::channel::oneshot;

#[derive(Clone, uniffi::Record)]
pub struct Config {
    pub revision: u64,
    pub limit: Option<u32>,
    pub nested: Nested,
}

#[derive(Clone, uniffi::Record)]
pub struct Nested {
    pub mode: Mode,
}

#[derive(Clone, uniffi::Enum)]
pub enum Mode {
    Off,
    Window { frames: u32 },
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[error("{self:?}")]
pub enum ProbeError {
    InvalidRevision,
    NotInitialized,
    InitializationInProgress,
    AlreadyInitialized,
    StateUnavailable,
    WaitInProgress,
    Cancelled,
}

#[must_use]
#[uniffi::export]
pub fn default_config() -> Config {
    Config {
        revision: 9_007_199_254_740_993,
        limit: Some(3),
        nested: Nested {
            mode: Mode::Window { frames: 128 },
        },
    }
}

/// Returns the supplied values after yielding.
///
/// # Errors
/// Rejects a zero revision.
#[uniffi::export]
pub async fn round_trip(config: Config) -> Result<Config, ProbeError> {
    let mut yielded = false;
    std::future::poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
    if config.revision == 0 {
        return Err(ProbeError::InvalidRevision);
    }
    Ok(config)
}

static LIVE: AtomicU32 = AtomicU32::new(0);

#[derive(uniffi::Object)]
pub struct Owner {
    config: Config,
}

#[uniffi::export]
impl Owner {
    #[uniffi::constructor]
    pub fn new(config: Config) -> Arc<Self> {
        LIVE.fetch_add(1, Ordering::Relaxed);
        Arc::new(Self { config })
    }
    #[must_use]
    pub fn values(&self) -> Config {
        self.config.clone()
    }
    pub fn notify(&self, listener: Box<dyn Listener>) {
        listener.changed(self.config.revision);
        drop(listener);
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

#[uniffi::export]
pub fn live_owners() -> u32 {
    LIVE.load(Ordering::Relaxed)
}

#[uniffi::export(callback_interface)]
pub trait Listener: Send + Sync {
    fn changed(&self, revision: u64);
}

static PENDING: Mutex<Option<oneshot::Sender<u64>>> = Mutex::new(None);

/// Waits for a producer in another browser worker and calls back locally.
///
/// # Errors
/// Rejects overlapping waits, unavailable state, and a cancelled producer.
#[uniffi::export]
pub async fn wait_for_worker(listener: Box<dyn Listener>) -> Result<u64, ProbeError> {
    let (tx, rx) = oneshot::channel();
    {
        let mut pending = PENDING.lock().map_err(|_| ProbeError::StateUnavailable)?;
        if pending.is_some() {
            return Err(ProbeError::WaitInProgress);
        }
        *pending = Some(tx);
    }
    #[cfg(target_arch = "wasm32")]
    let result = {
        let (local_tx, local_rx) = oneshot::channel();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = local_tx.send(rx.await);
        });
        local_rx.await.map_err(|_| ProbeError::Cancelled)?
    };
    #[cfg(not(target_arch = "wasm32"))]
    let result = rx.await;
    let value = result.map_err(|_| ProbeError::Cancelled)?;
    listener.changed(value);
    drop(listener);
    Ok(value)
}

/// Completes the pending cross-worker wait, if any.
///
/// # Errors
/// Rejects unavailable state.
#[uniffi::export]
pub fn release_from_worker() -> Result<bool, ProbeError> {
    let sender = PENDING
        .lock()
        .map_err(|_| ProbeError::StateUnavailable)?
        .take();
    Ok(sender.is_some_and(|tx| tx.send(9_007_199_254_740_993).is_ok()))
}

enum HostState {
    Uninitialized,
    Initializing,
    Ready(Config),
}
static HOST: Mutex<HostState> = Mutex::new(HostState::Uninitialized);

struct Initialization;
impl Drop for Initialization {
    fn drop(&mut self) {
        if let Ok(mut state) = HOST.lock()
            && matches!(*state, HostState::Initializing)
        {
            *state = HostState::Uninitialized;
        }
    }
}

/// Prepares the test host explicitly.
///
/// # Errors
/// Rejects invalid values, concurrent or repeated initialization, and poisoned state.
#[uniffi::export]
pub async fn initialize(config: Config) -> Result<(), ProbeError> {
    {
        let mut state = HOST.lock().map_err(|_| ProbeError::StateUnavailable)?;
        match *state {
            HostState::Initializing => return Err(ProbeError::InitializationInProgress),
            HostState::Ready(_) => return Err(ProbeError::AlreadyInitialized),
            HostState::Uninitialized => *state = HostState::Initializing,
        }
    }
    let _initialization = Initialization;
    let config = round_trip(config).await?;
    *HOST.lock().map_err(|_| ProbeError::StateUnavailable)? = HostState::Ready(config);
    Ok(())
}

/// Creates an independently owned player from the prepared host values.
///
/// # Errors
/// Rejects an uninitialized or unavailable host.
#[uniffi::export]
pub fn create_player() -> Result<Arc<Owner>, ProbeError> {
    match &*HOST.lock().map_err(|_| ProbeError::StateUnavailable)? {
        HostState::Ready(config) => Ok(Owner::new(config.clone())),
        HostState::Uninitialized | HostState::Initializing => Err(ProbeError::NotInitialized),
    }
}
