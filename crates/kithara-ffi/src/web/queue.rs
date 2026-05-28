use std::sync::LazyLock;

use js_sys::Promise;
use kithara_queue::{TrackId, Transition};
use num_traits::cast;
use wasm_bindgen::prelude::*;

use crate::web::{commands::WorkerCmd, inner::WasmInner};

/// Encode a [`TrackId`] as the JS `f64` the bindings hand back. Track ids
/// are allocated monotonically from `0` and stay far below `2^53`, so the
/// conversion is exact; `0.0` is a safe floor for the impossible overflow.
fn id_to_f64(id: TrackId) -> f64 {
    cast::<u64, f64>(id.as_u64()).unwrap_or(0.0)
}

/// Decode a [`TrackId`] from the JS `f64` convention. Non-finite or
/// negative inputs map to `TrackId(0)`; callers that need "no id" use
/// [`decode_optional_id`].
fn id_from_f64(raw: f64) -> TrackId {
    TrackId(cast::<f64, u64>(raw.max(0.0)).unwrap_or(0))
}

/// Process-wide [`WasmInner`] singleton backing the multi-track `queue_*`
/// wasm-bindgen surface. Parallel to the legacy
/// [`Player`](crate::web::player) singleton: both drive the same engine
/// worker (which owns the `Arc<Queue>`), but through separate command
/// entry points until Wave 4 merges them behind the
/// [`AudioPlayer`](crate::player) facade.
static QUEUE: LazyLock<WasmInner> = LazyLock::new(WasmInner::default);

fn queue() -> &'static WasmInner {
    &QUEUE
}

/// Boot the engine worker. Resolves once the worker spawn is requested.
#[wasm_bindgen]
pub fn queue_new() -> Promise {
    match queue().start() {
        Ok(()) => Promise::resolve(&JsValue::UNDEFINED),
        Err(err) => Promise::reject(&err),
    }
}

/// Append a track to the tail of the queue. Returns the allocated track id.
///
/// # Errors
/// Returns a JS error if the engine command channel is unavailable.
#[wasm_bindgen]
pub fn queue_append(url: String) -> Result<f64, JsValue> {
    let id = TrackId::allocate();
    queue().dispatch(WorkerCmd::Append { id, url })?;
    Ok(id_to_f64(id))
}

/// Insert a track after `after_id` (or at the head when `after_id` is
/// negative). Returns a `Promise` that resolves with the new track id once
/// the worker confirms placement, or rejects on `UnknownTrackId`.
#[wasm_bindgen]
pub fn queue_insert(url: String, after_id: f64) -> Result<Promise, JsValue> {
    let id = TrackId::allocate();
    let after = decode_optional_id(after_id);
    let request_id = crate::web::js::next_request_id();
    queue().dispatch(WorkerCmd::Insert {
        id,
        url,
        after,
        request_id,
    })?;
    crate::web::js::reply_promise_with_value(request_id, id_to_f64(id))
}

/// Remove a track by id. Returns a `Promise` that resolves once the worker
/// confirms removal, or rejects on `UnknownTrackId`.
#[wasm_bindgen]
pub fn queue_remove(id: f64) -> Result<Promise, JsValue> {
    let id = id_from_f64(id);
    let request_id = crate::web::js::next_request_id();
    queue().dispatch(WorkerCmd::Remove { id, request_id })?;
    crate::web::js::reply_promise(request_id)
}

/// Replace the track at `index`. Returns a `Promise` resolving with the new
/// track id, or rejecting if the index is out of range.
#[wasm_bindgen]
pub fn queue_replace(index: u32, url: String) -> Result<Promise, JsValue> {
    let id = TrackId::allocate();
    let request_id = crate::web::js::next_request_id();
    queue().dispatch(WorkerCmd::Replace {
        index,
        id,
        url,
        request_id,
    })?;
    crate::web::js::reply_promise_with_value(request_id, id_to_f64(id))
}

/// Select (start playing) a queued track with an immediate cut. Returns a
/// `Promise` resolving once the worker accepts the selection.
#[wasm_bindgen]
pub fn queue_select(id: f64) -> Result<Promise, JsValue> {
    let id = id_from_f64(id);
    let request_id = crate::web::js::next_request_id();
    queue().dispatch(WorkerCmd::SelectQueue {
        id,
        transition: Transition::None,
        request_id,
    })?;
    crate::web::js::reply_promise(request_id)
}

/// Remove every track from the queue.
///
/// # Errors
/// Returns a JS error if the engine command channel is unavailable.
#[wasm_bindgen]
pub fn queue_remove_all() -> Result<(), JsValue> {
    queue().dispatch(WorkerCmd::RemoveAll)
}

/// Start playback.
///
/// # Errors
/// Returns a JS error if the engine command channel is unavailable.
#[wasm_bindgen]
pub fn queue_play() -> Result<(), JsValue> {
    queue().play()
}

/// Pause playback.
///
/// # Errors
/// Returns a JS error if the engine command channel is unavailable.
#[wasm_bindgen]
pub fn queue_pause() -> Result<(), JsValue> {
    queue().pause()
}

/// Pause and clear the queue.
///
/// # Errors
/// Returns a JS error if the engine command channel is unavailable.
#[wasm_bindgen]
pub fn queue_stop() -> Result<(), JsValue> {
    queue().stop()
}

/// Seek the current track to `position_ms`.
///
/// # Errors
/// Returns a JS error if the engine command channel is unavailable.
#[wasm_bindgen]
pub fn queue_seek(position_ms: f64) -> Result<(), JsValue> {
    /// Milliseconds per second.
    const MS_PER_SECOND: f64 = 1000.0;
    queue().seek(position_ms / MS_PER_SECOND)
}

/// Set the output volume (0.0..=1.0).
///
/// # Errors
/// Returns a JS error if the engine command channel is unavailable.
#[wasm_bindgen]
pub fn queue_set_volume(volume: f32) -> Result<(), JsValue> {
    queue().set_volume(volume)
}

/// Set the crossfade duration in seconds.
///
/// # Errors
/// Returns a JS error if the engine command channel is unavailable.
#[wasm_bindgen]
pub fn queue_set_crossfade_seconds(seconds: f32) -> Result<(), JsValue> {
    queue().set_crossfade_duration(seconds)
}

/// Set the gain for an EQ band.
///
/// # Errors
/// Returns a JS error if the engine command channel is unavailable.
#[wasm_bindgen]
pub fn queue_set_eq_gain(band: u32, gain_db: f32) -> Result<(), JsValue> {
    queue().set_eq_gain(band, gain_db)
}

/// Reset every EQ band to 0 dB.
///
/// # Errors
/// Returns a JS error if the engine command channel is unavailable.
#[wasm_bindgen]
pub fn queue_reset_eq() -> Result<(), JsValue> {
    queue().reset_eq()
}

/// Decode an optional `TrackId` from the JS `f64` convention: negative
/// means `None` (insert at head), non-negative is the id.
fn decode_optional_id(raw: f64) -> Option<TrackId> {
    if raw < 0.0 {
        None
    } else {
        Some(id_from_f64(raw))
    }
}
