use js_sys::Object;
use wasm_bindgen::JsValue;

use super::marshal::{KIND, set_bool, set_f64, set_opt_f64, set_opt_id, set_str};
use crate::types::{
    FfiAdvanceReason, FfiEvictReason, FfiPlayerEvent, FfiPlayerStatus, FfiRepeatMode,
    FfiRouteChangeReason, FfiStretchBackendKind, FfiTimeControlStatus, FfiTrackStatus,
};

pub(crate) fn encode(event: &FfiPlayerEvent) -> JsValue {
    let obj = Object::new();
    match event {
        FfiPlayerEvent::TimeChanged { seconds } => {
            set_str(&obj, KIND, "TimeChanged");
            set_f64(&obj, "seconds", *seconds);
        }
        FfiPlayerEvent::RateChanged { rate } => {
            set_str(&obj, KIND, "RateChanged");
            set_f64(&obj, "rate", f64::from(*rate));
        }
        FfiPlayerEvent::CurrentItemChanged { item_id } => {
            set_str(&obj, KIND, "CurrentItemChanged");
            set_opt_id(&obj, "item_id", *item_id);
        }
        FfiPlayerEvent::StatusChanged { status } => {
            set_str(&obj, KIND, "StatusChanged");
            set_f64(&obj, "status", player_status_code(*status));
        }
        FfiPlayerEvent::TimeControlStatusChanged { status } => {
            set_str(&obj, KIND, "TimeControlStatusChanged");
            set_f64(&obj, "status", time_control_code(*status));
        }
        FfiPlayerEvent::Error { error } => {
            set_str(&obj, KIND, "Error");
            set_str(&obj, "error", error);
        }
        FfiPlayerEvent::DurationChanged { seconds } => {
            set_str(&obj, KIND, "DurationChanged");
            set_f64(&obj, "seconds", *seconds);
        }
        FfiPlayerEvent::BufferedDurationChanged { seconds } => {
            set_str(&obj, KIND, "BufferedDurationChanged");
            set_f64(&obj, "seconds", *seconds);
        }
        FfiPlayerEvent::VolumeChanged { volume } => {
            set_str(&obj, KIND, "VolumeChanged");
            set_f64(&obj, "volume", f64::from(*volume));
        }
        FfiPlayerEvent::MuteChanged { muted } => {
            set_str(&obj, KIND, "MuteChanged");
            set_bool(&obj, "muted", *muted);
        }
        FfiPlayerEvent::ItemDidPlayToEnd => set_str(&obj, KIND, "ItemDidPlayToEnd"),
        FfiPlayerEvent::ItemDidFail { item_id } => {
            set_str(&obj, KIND, "ItemDidFail");
            set_opt_id(&obj, "item_id", *item_id);
        }
        FfiPlayerEvent::TrackStatusChanged { item_id, status } => {
            set_str(&obj, KIND, "TrackStatusChanged");
            set_f64(
                &obj,
                "item_id",
                num_traits::cast(item_id.as_u64()).unwrap_or(0.0),
            );
            encode_track_status(&obj, status);
        }
        FfiPlayerEvent::QueueEnded => set_str(&obj, KIND, "QueueEnded"),
        FfiPlayerEvent::CrossfadeStarted { duration_seconds } => {
            set_str(&obj, KIND, "CrossfadeStarted");
            set_f64(&obj, "seconds", f64::from(*duration_seconds));
        }
        FfiPlayerEvent::CrossfadeDurationChanged { seconds } => {
            set_str(&obj, KIND, "CrossfadeDurationChanged");
            set_f64(&obj, "seconds", f64::from(*seconds));
        }
        FfiPlayerEvent::TrackAdded { item_id, index } => {
            set_str(&obj, KIND, "TrackAdded");
            set_f64(
                &obj,
                "item_id",
                num_traits::cast(item_id.as_u64()).unwrap_or(0.0),
            );
            set_f64(&obj, "index", num_traits::cast(*index).unwrap_or(0.0));
        }
        FfiPlayerEvent::TrackRemoved { item_id } => {
            set_str(&obj, KIND, "TrackRemoved");
            set_f64(
                &obj,
                "item_id",
                num_traits::cast(item_id.as_u64()).unwrap_or(0.0),
            );
        }
        FfiPlayerEvent::TrackLoadFailed {
            item_id,
            reason,
            auto_skipped,
        } => {
            set_str(&obj, KIND, "TrackLoadFailed");
            set_f64(
                &obj,
                "item_id",
                num_traits::cast(item_id.as_u64()).unwrap_or(0.0),
            );
            set_str(&obj, "reason", reason);
            set_bool(&obj, "auto_skipped", *auto_skipped);
        }
        FfiPlayerEvent::RepeatModeChanged { mode } => {
            set_str(&obj, KIND, "RepeatModeChanged");
            set_str(&obj, "mode", repeat_mode_str(*mode));
        }
        FfiPlayerEvent::NextTrackReady { item_id, index } => {
            set_str(&obj, KIND, "NextTrackReady");
            set_f64(
                &obj,
                "item_id",
                num_traits::cast(item_id.as_u64()).unwrap_or(0.0),
            );
            set_f64(&obj, "index", num_traits::cast(*index).unwrap_or(0.0));
        }
        FfiPlayerEvent::CurrentItemAdvanced { item_id, reason } => {
            set_str(&obj, KIND, "CurrentItemAdvanced");
            set_opt_id(&obj, "item_id", *item_id);
            set_str(&obj, "reason", advance_reason_str(*reason));
        }
        FfiPlayerEvent::EngineStarted => set_str(&obj, KIND, "EngineStarted"),
        FfiPlayerEvent::EngineStopped => set_str(&obj, KIND, "EngineStopped"),
        FfiPlayerEvent::CrossfadeCompleted => set_str(&obj, KIND, "CrossfadeCompleted"),
        FfiPlayerEvent::CrossfadeCancelled => set_str(&obj, KIND, "CrossfadeCancelled"),
        FfiPlayerEvent::MasterVolumeChanged { volume } => {
            set_str(&obj, KIND, "MasterVolumeChanged");
            set_f64(&obj, "volume", f64::from(*volume));
        }
        FfiPlayerEvent::AudioRouteChanged { reason } => {
            set_str(&obj, KIND, "AudioRouteChanged");
            set_str(&obj, "reason", route_change_reason_str(*reason));
        }
        FfiPlayerEvent::DjBpmDetected {
            slot,
            bpm,
            confidence,
            first_beat_offset_seconds,
        } => {
            set_str(&obj, KIND, "DjBpmDetected");
            set_f64(&obj, "slot", num_traits::cast(*slot).unwrap_or(0.0));
            set_f64(&obj, "bpm", *bpm);
            set_opt_f64(&obj, "confidence", confidence.map(f64::from));
            set_f64(
                &obj,
                "first_beat_offset_seconds",
                *first_beat_offset_seconds,
            );
        }
        FfiPlayerEvent::DjKeylockChanged { on } => {
            set_str(&obj, KIND, "DjKeylockChanged");
            set_bool(&obj, "on", *on);
        }
        FfiPlayerEvent::DjStretchBackendChanged { kind } => {
            set_str(&obj, KIND, "DjStretchBackendChanged");
            set_str(&obj, "kind", stretch_backend_kind_str(*kind));
        }
        FfiPlayerEvent::AssetCommitted {
            asset_root,
            rel_path,
            final_len,
        } => {
            set_str(&obj, KIND, "AssetCommitted");
            set_str(&obj, "asset_root", asset_root);
            set_str(&obj, "rel_path", rel_path);
            set_opt_f64(
                &obj,
                "final_len",
                final_len.and_then(|len| num_traits::cast(len)),
            );
        }
        FfiPlayerEvent::AssetFailed {
            asset_root,
            rel_path,
            reason,
        } => {
            set_str(&obj, KIND, "AssetFailed");
            set_str(&obj, "asset_root", asset_root);
            set_str(&obj, "rel_path", rel_path);
            set_str(&obj, "reason", reason);
        }
        FfiPlayerEvent::AssetEvicted { asset_root, reason } => {
            set_str(&obj, KIND, "AssetEvicted");
            set_str(&obj, "asset_root", asset_root);
            set_str(&obj, "reason", evict_reason_str(*reason));
        }
    }
    obj.into()
}

fn encode_track_status(obj: &Object, status: &FfiTrackStatus) {
    match status {
        FfiTrackStatus::Pending => set_f64(obj, "status", 0.0),
        FfiTrackStatus::Loading => set_f64(obj, "status", 1.0),
        FfiTrackStatus::Slow => set_f64(obj, "status", 2.0),
        FfiTrackStatus::Loaded => set_f64(obj, "status", 3.0),
        FfiTrackStatus::Failed { reason } => {
            set_f64(obj, "status", 4.0);
            set_str(obj, "reason", reason);
        }
        FfiTrackStatus::Consumed => set_f64(obj, "status", 5.0),
        FfiTrackStatus::Cancelled => set_f64(obj, "status", 6.0),
    }
}

fn player_status_code(status: FfiPlayerStatus) -> f64 {
    match status {
        FfiPlayerStatus::Unknown => 0.0,
        FfiPlayerStatus::ReadyToPlay => 1.0,
        FfiPlayerStatus::Failed => 2.0,
    }
}

fn time_control_code(status: FfiTimeControlStatus) -> f64 {
    match status {
        FfiTimeControlStatus::Paused => 0.0,
        FfiTimeControlStatus::WaitingToPlay => 1.0,
        FfiTimeControlStatus::Playing => 2.0,
    }
}

fn advance_reason_str(reason: FfiAdvanceReason) -> &'static str {
    match reason {
        FfiAdvanceReason::NaturalEof => "NaturalEof",
        FfiAdvanceReason::CrossfadePreArm => "CrossfadePreArm",
        FfiAdvanceReason::UserSelect => "UserSelect",
        FfiAdvanceReason::UserNext => "UserNext",
        FfiAdvanceReason::UserPrev => "UserPrev",
        FfiAdvanceReason::TrackFailed => "TrackFailed",
        FfiAdvanceReason::RemovedCurrent => "RemovedCurrent",
        FfiAdvanceReason::Repeat => "Repeat",
        FfiAdvanceReason::Cancelled => "Cancelled",
        FfiAdvanceReason::Unknown => "Unknown",
    }
}

fn repeat_mode_str(mode: FfiRepeatMode) -> &'static str {
    match mode {
        FfiRepeatMode::Off => "Off",
        FfiRepeatMode::One => "One",
        FfiRepeatMode::All => "All",
        FfiRepeatMode::Unknown => "Unknown",
    }
}

fn route_change_reason_str(reason: FfiRouteChangeReason) -> &'static str {
    match reason {
        FfiRouteChangeReason::NewDeviceAvailable => "NewDeviceAvailable",
        FfiRouteChangeReason::OldDeviceUnavailable => "OldDeviceUnavailable",
        FfiRouteChangeReason::CategoryChange => "CategoryChange",
        FfiRouteChangeReason::Override => "Override",
        FfiRouteChangeReason::WakeFromSleep => "WakeFromSleep",
        FfiRouteChangeReason::NoSuitableRouteForCategory => "NoSuitableRouteForCategory",
        FfiRouteChangeReason::RouteConfigurationChange => "RouteConfigurationChange",
        FfiRouteChangeReason::Unknown => "Unknown",
    }
}

fn stretch_backend_kind_str(kind: FfiStretchBackendKind) -> &'static str {
    match kind {
        FfiStretchBackendKind::Signalsmith => "Signalsmith",
        FfiStretchBackendKind::Bungee => "Bungee",
        FfiStretchBackendKind::Unknown => "Unknown",
    }
}

fn evict_reason_str(reason: FfiEvictReason) -> &'static str {
    match reason {
        FfiEvictReason::QuotaBytes => "QuotaBytes",
        FfiEvictReason::QuotaAssets => "QuotaAssets",
        FfiEvictReason::Displaced => "Displaced",
        FfiEvictReason::Unknown => "Unknown",
    }
}
