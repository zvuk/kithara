use js_sys::Reflect;
use kithara_events::TrackId;
use kithara_platform::sync::{Arc, Mutex};
use wasm_bindgen::{JsCast, JsValue, prelude::Closure};
use web_sys::{BroadcastChannel, MessageEvent};

use crate::{
    item::AudioPlayerItem,
    observer::{ItemObserver, PlayerObserver},
    types::{
        FfiAdvanceReason, FfiEvictReason, FfiItemEvent, FfiItemStatus, FfiPlayerEvent,
        FfiPlayerStatus, FfiRepeatMode, FfiRouteChangeReason, FfiStretchBackendKind,
        FfiTimeControlStatus, FfiTrackStatus,
    },
    web::observer::source::EVENT_CHANNEL,
};

/// Caller-facing ordered queue view shared with
/// [`WasmInner`](crate::web::inner::WasmInner). Lets the router map a
/// [`TrackId`] back to its [`AudioPlayerItem`] for per-item observer
/// dispatch.
type QueueView = Vec<(TrackId, Arc<AudioPlayerItem>)>;

/// Listen on [`EVENT_CHANNEL`] on the main thread and dispatch decoded
/// [`FfiPlayerEvent`]s to `observer`, plus per-item
/// [`FfiItemEvent`]s to the matching item's [`ItemObserver`]. Replaces
/// any previously-installed router (last `set_observer` wins).
pub(crate) fn install(observer: Arc<dyn PlayerObserver>, queue_view: Arc<Mutex<QueueView>>) {
    let Ok(channel) = BroadcastChannel::new(EVENT_CHANNEL) else {
        web_sys::console::warn_1(&JsValue::from_str(
            "kithara: BroadcastChannel unavailable; player observer disabled",
        ));
        return;
    };
    let closure = Closure::wrap(Box::new(move |ev: MessageEvent| {
        let data = ev.data();
        let Some(event) = decode(&data) else {
            return;
        };
        route_to_item(&queue_view, &event);
        observer.on_event(event);
    }) as Box<dyn FnMut(MessageEvent)>);
    channel.set_onmessage(Some(closure.as_ref().unchecked_ref()));
    closure.forget();
    std::mem::forget(channel);
}

/// Mirror native `route_track_status_to_item`: surface
/// `TrackStatusChanged` as per-item `StatusChanged` / `Error` callbacks,
/// and refresh the item's cached [`ItemState`](crate::item) so
/// [`AudioPlayerItem::load`](crate::item::AudioPlayerItem) answers
/// truthfully even when no observer is attached.
fn route_to_item(queue_view: &Arc<Mutex<QueueView>>, event: &FfiPlayerEvent) {
    let FfiPlayerEvent::TrackStatusChanged { item_id, status } = event else {
        return;
    };
    let item = queue_view
        .lock()
        .iter()
        .find(|(id, _)| id == item_id)
        .map(|(_, item)| Arc::clone(item));
    let Some(item) = item else { return };
    update_item_state(&item, status);
    if let Some(item_obs) = item.observer() {
        dispatch_track_status_to_item(&item_obs, status);
    }
}

fn update_item_state(item: &Arc<AudioPlayerItem>, status: &FfiTrackStatus) {
    match status {
        FfiTrackStatus::Loaded => {
            let duration = item.duration_sec();
            item.state.lock().resolve_duration(duration);
        }
        FfiTrackStatus::Failed { .. } => {
            item.state.lock().mark_failed();
        }
        _ => {}
    }
}

fn dispatch_track_status_to_item(observer: &Arc<dyn ItemObserver>, status: &FfiTrackStatus) {
    match status {
        FfiTrackStatus::Loaded => observer.on_event(FfiItemEvent::StatusChanged {
            status: FfiItemStatus::ReadyToPlay,
        }),
        FfiTrackStatus::Failed { reason } => {
            observer.on_event(FfiItemEvent::StatusChanged {
                status: FfiItemStatus::Failed,
            });
            observer.on_event(FfiItemEvent::Error {
                error: reason.clone(),
            });
        }
        _ => {}
    }
}

fn get_str(val: &JsValue, key: &str) -> Option<String> {
    Reflect::get(val, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_string())
}

fn get_f64(val: &JsValue, key: &str) -> Option<f64> {
    Reflect::get(val, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_f64())
}

fn get_bool(val: &JsValue, key: &str) -> Option<bool> {
    Reflect::get(val, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_bool())
}

fn get_opt_id(val: &JsValue, key: &str) -> Option<TrackId> {
    get_f64(val, key).map(|raw| TrackId(num_traits::cast(raw.max(0.0)).unwrap_or(0)))
}

fn get_id_req(val: &JsValue, key: &str) -> Option<TrackId> {
    Some(TrackId(
        num_traits::cast(get_f64(val, key)?.max(0.0)).unwrap_or(0),
    ))
}

/// Narrow a marshalled `f64` field back to `f32`. The worker encodes
/// `f32` settings widened to `f64`, so the value is in range; out-of-range
/// inputs (only possible from a malformed payload) clamp to `0.0`.
fn narrow_f32(value: f64) -> f32 {
    num_traits::cast(value).unwrap_or(0.0)
}

/// Decode a JS object produced by
/// [`events`](crate::web::observer::source)'s `encode`.
fn decode(data: &JsValue) -> Option<FfiPlayerEvent> {
    let kind = get_str(data, "kind")?;
    Some(match kind.as_str() {
        "TimeChanged" => FfiPlayerEvent::TimeChanged {
            seconds: get_f64(data, "seconds")?,
        },
        "RateChanged" => FfiPlayerEvent::RateChanged {
            rate: narrow_f32(get_f64(data, "rate")?),
        },
        "CurrentItemChanged" => FfiPlayerEvent::CurrentItemChanged {
            item_id: get_opt_id(data, "item_id"),
        },
        "StatusChanged" => FfiPlayerEvent::StatusChanged {
            status: decode_player_status(get_f64(data, "status")?),
        },
        "TimeControlStatusChanged" => FfiPlayerEvent::TimeControlStatusChanged {
            status: decode_time_control(get_f64(data, "status")?),
        },
        "Error" => FfiPlayerEvent::Error {
            error: get_str(data, "error").unwrap_or_default(),
        },
        "DurationChanged" => FfiPlayerEvent::DurationChanged {
            seconds: get_f64(data, "seconds")?,
        },
        "BufferedDurationChanged" => FfiPlayerEvent::BufferedDurationChanged {
            seconds: get_f64(data, "seconds")?,
        },
        "VolumeChanged" => FfiPlayerEvent::VolumeChanged {
            volume: narrow_f32(get_f64(data, "volume")?),
        },
        "MuteChanged" => FfiPlayerEvent::MuteChanged {
            muted: get_bool(data, "muted")?,
        },
        "ItemDidPlayToEnd" => FfiPlayerEvent::ItemDidPlayToEnd,
        "ItemDidFail" => FfiPlayerEvent::ItemDidFail {
            item_id: get_opt_id(data, "item_id"),
        },
        "TrackStatusChanged" => FfiPlayerEvent::TrackStatusChanged {
            item_id: get_id_req(data, "item_id")?,
            status: decode_track_status(data),
        },
        "QueueEnded" => FfiPlayerEvent::QueueEnded,
        "CrossfadeStarted" => FfiPlayerEvent::CrossfadeStarted {
            duration_seconds: narrow_f32(get_f64(data, "seconds")?),
        },
        "CrossfadeDurationChanged" => FfiPlayerEvent::CrossfadeDurationChanged {
            seconds: narrow_f32(get_f64(data, "seconds")?),
        },
        "TrackAdded" => FfiPlayerEvent::TrackAdded {
            item_id: get_id_req(data, "item_id")?,
            index: num_traits::cast(get_f64(data, "index")?).unwrap_or(0),
        },
        "TrackRemoved" => FfiPlayerEvent::TrackRemoved {
            item_id: get_id_req(data, "item_id")?,
        },
        "TrackLoadFailed" => FfiPlayerEvent::TrackLoadFailed {
            item_id: get_id_req(data, "item_id")?,
            reason: get_str(data, "reason").unwrap_or_default(),
            auto_skipped: get_bool(data, "auto_skipped")?,
        },
        "RepeatModeChanged" => FfiPlayerEvent::RepeatModeChanged {
            mode: decode_repeat_mode(get_str(data, "mode")),
        },
        "NextTrackReady" => FfiPlayerEvent::NextTrackReady {
            item_id: get_id_req(data, "item_id")?,
            index: num_traits::cast(get_f64(data, "index")?).unwrap_or(0),
        },
        "CurrentItemAdvanced" => FfiPlayerEvent::CurrentItemAdvanced {
            item_id: get_opt_id(data, "item_id"),
            reason: decode_advance_reason(get_str(data, "reason")),
        },
        "EngineStarted" => FfiPlayerEvent::EngineStarted,
        "EngineStopped" => FfiPlayerEvent::EngineStopped,
        "CrossfadeCompleted" => FfiPlayerEvent::CrossfadeCompleted,
        "CrossfadeCancelled" => FfiPlayerEvent::CrossfadeCancelled,
        "MasterVolumeChanged" => FfiPlayerEvent::MasterVolumeChanged {
            volume: narrow_f32(get_f64(data, "volume")?),
        },
        "AudioRouteChanged" => FfiPlayerEvent::AudioRouteChanged {
            reason: decode_route_change_reason(get_str(data, "reason")),
        },
        "DjBpmDetected" => FfiPlayerEvent::DjBpmDetected {
            slot: num_traits::cast(get_f64(data, "slot")?).unwrap_or(0),
            bpm: get_f64(data, "bpm")?,
            confidence: get_f64(data, "confidence").map(narrow_f32),
            first_beat_offset_seconds: get_f64(data, "first_beat_offset_seconds")?,
        },
        "DjKeylockChanged" => FfiPlayerEvent::DjKeylockChanged {
            on: get_bool(data, "on")?,
        },
        "DjStretchBackendChanged" => FfiPlayerEvent::DjStretchBackendChanged {
            kind: decode_stretch_backend_kind(get_str(data, "kind")),
        },
        "AssetCommitted" => FfiPlayerEvent::AssetCommitted {
            asset_root: get_str(data, "asset_root").unwrap_or_default(),
            rel_path: get_str(data, "rel_path").unwrap_or_default(),
            final_len: get_f64(data, "final_len").map(|value| num_traits::cast(value).unwrap_or(0)),
        },
        "AssetFailed" => FfiPlayerEvent::AssetFailed {
            asset_root: get_str(data, "asset_root").unwrap_or_default(),
            rel_path: get_str(data, "rel_path").unwrap_or_default(),
            reason: get_str(data, "reason").unwrap_or_default(),
        },
        "AssetEvicted" => FfiPlayerEvent::AssetEvicted {
            asset_root: get_str(data, "asset_root").unwrap_or_default(),
            reason: decode_evict_reason(get_str(data, "reason")),
        },
        _ => return None,
    })
}

/// Decode a small non-negative discriminant from a marshalled `f64`.
/// Out-of-range / non-finite inputs map to `0` (the "unknown" arm).
fn discriminant(code: f64) -> u32 {
    num_traits::cast(code).unwrap_or(0)
}

fn decode_player_status(code: f64) -> FfiPlayerStatus {
    match discriminant(code) {
        1 => FfiPlayerStatus::ReadyToPlay,
        2 => FfiPlayerStatus::Failed,
        _ => FfiPlayerStatus::Unknown,
    }
}

fn decode_time_control(code: f64) -> FfiTimeControlStatus {
    match discriminant(code) {
        1 => FfiTimeControlStatus::WaitingToPlay,
        2 => FfiTimeControlStatus::Playing,
        _ => FfiTimeControlStatus::Paused,
    }
}

fn decode_track_status(data: &JsValue) -> FfiTrackStatus {
    match discriminant(get_f64(data, "status").unwrap_or(0.0)) {
        1 => FfiTrackStatus::Loading,
        2 => FfiTrackStatus::Slow,
        3 => FfiTrackStatus::Loaded,
        4 => FfiTrackStatus::Failed {
            reason: get_str(data, "reason").unwrap_or_default(),
        },
        5 => FfiTrackStatus::Consumed,
        6 => FfiTrackStatus::Cancelled,
        _ => FfiTrackStatus::Pending,
    }
}

fn decode_advance_reason(value: Option<String>) -> FfiAdvanceReason {
    match value.as_deref() {
        Some("NaturalEof") => FfiAdvanceReason::NaturalEof,
        Some("CrossfadePreArm") => FfiAdvanceReason::CrossfadePreArm,
        Some("UserSelect") => FfiAdvanceReason::UserSelect,
        Some("UserNext") => FfiAdvanceReason::UserNext,
        Some("UserPrev") => FfiAdvanceReason::UserPrev,
        Some("TrackFailed") => FfiAdvanceReason::TrackFailed,
        Some("RemovedCurrent") => FfiAdvanceReason::RemovedCurrent,
        Some("Repeat") => FfiAdvanceReason::Repeat,
        Some("Cancelled") => FfiAdvanceReason::Cancelled,
        _ => FfiAdvanceReason::Unknown,
    }
}

fn decode_repeat_mode(value: Option<String>) -> FfiRepeatMode {
    match value.as_deref() {
        Some("Off") => FfiRepeatMode::Off,
        Some("One") => FfiRepeatMode::One,
        Some("All") => FfiRepeatMode::All,
        _ => FfiRepeatMode::Unknown,
    }
}

fn decode_route_change_reason(value: Option<String>) -> FfiRouteChangeReason {
    match value.as_deref() {
        Some("NewDeviceAvailable") => FfiRouteChangeReason::NewDeviceAvailable,
        Some("OldDeviceUnavailable") => FfiRouteChangeReason::OldDeviceUnavailable,
        Some("CategoryChange") => FfiRouteChangeReason::CategoryChange,
        Some("Override") => FfiRouteChangeReason::Override,
        Some("WakeFromSleep") => FfiRouteChangeReason::WakeFromSleep,
        Some("NoSuitableRouteForCategory") => FfiRouteChangeReason::NoSuitableRouteForCategory,
        Some("RouteConfigurationChange") => FfiRouteChangeReason::RouteConfigurationChange,
        _ => FfiRouteChangeReason::Unknown,
    }
}

fn decode_stretch_backend_kind(value: Option<String>) -> FfiStretchBackendKind {
    match value.as_deref() {
        Some("Signalsmith") => FfiStretchBackendKind::Signalsmith,
        Some("Bungee") => FfiStretchBackendKind::Bungee,
        _ => FfiStretchBackendKind::Unknown,
    }
}

fn decode_evict_reason(value: Option<String>) -> FfiEvictReason {
    match value.as_deref() {
        Some("QuotaBytes") => FfiEvictReason::QuotaBytes,
        Some("QuotaAssets") => FfiEvictReason::QuotaAssets,
        Some("Displaced") => FfiEvictReason::Displaced,
        _ => FfiEvictReason::Unknown,
    }
}
