use std::collections::HashMap;

use kithara_queue::{TrackId, Transition};

/// Commands sent from the main-thread bridge to the engine Worker.
///
/// The multi-track queue variants
/// (`Append`/`Insert`/`Remove`/`Replace`/`SelectQueue`/`RemoveAll`)
/// mirror [`NativeInner`](crate::native::inner::NativeInner)'s queue
/// methods — the caller allocates the [`TrackId`] on the main thread
/// (shared-memory atomic, so ids stay process-monotonic across the
/// worker boundary) and the worker plants it via `append_with_id` /
/// `insert_with_id`.
#[derive(Clone)]
pub(crate) enum WorkerCmd {
    Play,
    Pause,
    Stop,
    Seek(f64),
    SetVolume(f32),
    SetCrossfade(f32),
    SetEqGain {
        band: u32,
        gain_db: f32,
    },
    ResetEq,
    /// Append a track to the tail of the queue. Loading starts in the
    /// background; playback does not begin until a matching `SelectQueue`.
    Append {
        id: TrackId,
        url: String,
    },
    /// Insert a track after `after` (or at the head when `after` is
    /// `None`). Replies via `request_id` so the caller can observe the
    /// `UnknownTrackId` rejection.
    Insert {
        id: TrackId,
        url: String,
        after: Option<TrackId>,
        request_id: u32,
    },
    /// Remove a track by id. Replies via `request_id`.
    Remove {
        id: TrackId,
        request_id: u32,
    },
    /// Replace the track at `index`. Replies via `request_id`.
    Replace {
        index: u32,
        id: TrackId,
        url: String,
        request_id: u32,
    },
    /// Select (start playing) a queued track. Replies via `request_id`.
    SelectQueue {
        id: TrackId,
        transition: Transition,
        request_id: u32,
    },
    /// Clear every track from the queue.
    RemoveAll,
    /// Set the ABR mode on the worker's current track. `None` selects
    /// automatic adaptation; `Some(index)` pins a manual variant. Mirrors
    /// [`NativeInner::set_abr_mode`](crate::native::inner::NativeInner).
    SetAbrMode {
        variant_index: Option<u32>,
    },
    /// Apply per-network peak-bitrate ceilings to the worker's current ABR
    /// handle. Mirrors
    /// [`NativeInner::update_peak_bitrate`](crate::native::inner::NativeInner).
    PeakBitrate {
        wifi_bps: f64,
        cellular_bps: f64,
    },
    /// Set or clear the player-wide auth token. An empty token removes the
    /// `X-Auth-Token` header. Applied to subsequently built tracks. Mirrors
    /// [`NativeInner::setup_network`](crate::native::inner::NativeInner).
    AuthToken {
        token: String,
    },
    /// Register a DRM key rule on the worker. The worker builds a
    /// cross-thread key processor (the real JS callback stays on the main
    /// thread) keyed on `salt`, then folds the rule into the player-wide
    /// `KeyOptions` and header map. Mirrors
    /// [`NativeInner::setup_hls_aes_with_rule`](crate::native::inner::NativeInner).
    SetupHlsAes {
        salt: String,
        domains: Vec<String>,
        headers: Option<HashMap<String, String>>,
        query_params: Option<HashMap<String, String>>,
    },
}
