use std::sync::LazyLock;

use bytes::Bytes;
use kithara_drm::{DrmError, KeyProcessResult, KeyProcessor};
use kithara_platform::{
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use crate::observer::FfiKeyProcessor;

/// One key-decrypt request crossing from the engine Worker to the main
/// thread. The worker fills `key` + `salt` and a one-shot `reply` sender,
/// then blocks on the matching receiver until the main thread pump posts
/// the decrypted bytes back.
struct KeyMsg {
    reply: mpsc::Sender<Vec<u8>>,
    salt: String,
    key: Vec<u8>,
}

/// Shared state for the cross-thread DRM key round-trip. Lives behind a
/// single process-wide [`LazyLock`] so the main thread (which installs the
/// JS processor and drains requests) and the engine Worker (which posts
/// requests from its `KeyProcessor` closure) reach the same channel.
struct KeyBridge {
    /// JS key callback registered on the main thread by `setup_hls_aes`.
    /// Held behind a `SendWrapper` (inside the JS shim) so the `!Send` JS
    /// `Function` can live here while only ever being invoked on the
    /// installing thread. `None` until the first `setup_hls_aes`.
    processor: Mutex<Option<Arc<dyn FfiKeyProcessor>>>,
    /// Receiver half, drained by [`pump`] on the main thread.
    request_rx: Mutex<Option<mpsc::Receiver<KeyMsg>>>,
    /// Worker → main key-request channel sender; the worker clones it.
    request_tx: Mutex<Option<mpsc::Sender<KeyMsg>>>,
}

static BRIDGE: LazyLock<KeyBridge> = LazyLock::new(|| KeyBridge {
    request_tx: Mutex::new(None),
    request_rx: Mutex::new(None),
    processor: Mutex::new(None),
});

/// Register the main-thread JS key processor and (idempotently) create the
/// worker → main key-request channel. Called from
/// [`WasmInner::setup_hls_aes`](crate::web::inner::WasmInner) on the main
/// thread. Replacing the processor (a later `setup_hls_aes`) keeps the same
/// channel — only the callback target changes.
pub(crate) fn install_main_processor(processor: Arc<dyn FfiKeyProcessor>) {
    *BRIDGE.processor.lock() = Some(processor);
    let mut tx_guard = BRIDGE.request_tx.lock();
    if tx_guard.is_none() {
        let (tx, rx) = mpsc::channel();
        *tx_guard = Some(tx);
        *BRIDGE.request_rx.lock() = Some(rx);
    }
}

/// Drain every pending key request on the main thread, invoke the
/// registered JS processor synchronously, and reply with the decrypted
/// bytes. Must run on the main thread (it touches the `!Send` JS callback);
/// call it from the same `requestAnimationFrame` loop that drives session
/// polling. A no-op when no processor is installed or no request is
/// pending.
pub(crate) fn pump() {
    let rx_guard = BRIDGE.request_rx.lock();
    let Some(rx) = rx_guard.as_ref() else {
        return;
    };
    for msg in rx.try_iter() {
        let bytes = BRIDGE
            .processor
            .lock()
            .as_ref()
            .map(|p| p.process_key(msg.key, msg.salt))
            .unwrap_or_default();
        let _ = msg.reply.send(bytes);
    }
}

/// Build the worker-side [`KeyProcessor`] closure for `salt`. Invoked
/// synchronously by `kithara-drm` on the Worker thread during segment
/// decrypt: it posts the raw key + salt to the main thread and blocks
/// (bounded by a wall-clock deadline) for the decrypted reply. The deadline
/// guards against a stalled main thread — a timeout surfaces a typed
/// [`DrmError`] so the decrypt fails loudly instead of hanging.
pub(crate) fn worker_key_processor(salt: String) -> KeyProcessor {
    /// Wall-clock deadline for one cross-thread key round-trip. Generous
    /// because the main thread only services key requests on its
    /// `requestAnimationFrame` pump cadence.
    const KEY_ROUNDTRIP_TIMEOUT: Duration = Duration::from_secs(10);

    Arc::new(move |key: Bytes| -> KeyProcessResult {
        let tx = BRIDGE
            .request_tx
            .lock()
            .as_ref()
            .cloned()
            .ok_or_else(|| DrmError::KeyProcessing("key bridge not initialised".into()))?;

        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send(KeyMsg {
            key: key.to_vec(),
            salt: salt.clone(),
            reply: reply_tx,
        })
        .map_err(|_| DrmError::KeyProcessing("key bridge main thread gone".into()))?;

        let deadline = Instant::now() + KEY_ROUNDTRIP_TIMEOUT;
        match reply_rx.recv_timeout(deadline) {
            Ok(bytes) => Ok(Bytes::from(bytes)),
            Err(err) => Err(DrmError::KeyProcessing(format!(
                "key reply not received within deadline: {err:?}"
            ))),
        }
    })
}
