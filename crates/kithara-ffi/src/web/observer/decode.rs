use wasm_bindgen::JsValue;

use super::marshal::{
    discriminant, get_bool, get_f64, get_id_req, get_opt_id, get_str, narrow_f32,
};
use crate::types::{
    FfiAdvanceReason, FfiEvictReason, FfiPlayerEvent, FfiPlayerStatus, FfiRepeatMode,
    FfiRouteChangeReason, FfiStretchBackendKind, FfiTimeControlStatus, FfiTrackStatus,
};

pub(crate) fn decode(data: &JsValue) -> Option<FfiPlayerEvent> {
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
