use kithara_queue::{TrackId, Transition};

/// Commands sent from the main-thread bridge to the engine Worker.
///
/// The legacy single-track variants (`SelectTrack`, `Play`, `Pause`, …)
/// remain in place: the demo's `player_*` free functions still drive
/// them. Wave 4 merges the overlapping control surface; Wave 6 removes
/// the legacy half. The multi-track queue variants
/// (`Append`/`Insert`/`Remove`/`Replace`/`SelectQueue`/`RemoveAll`)
/// mirror [`NativeInner`](crate::native::inner::NativeInner)'s queue
/// methods — the caller allocates the [`TrackId`] on the main thread
/// (shared-memory atomic, so ids stay process-monotonic across the
/// worker boundary) and the worker plants it via `append_with_id` /
/// `insert_with_id`.
#[derive(Clone)]
pub(crate) enum WorkerCmd {
    SelectTrack {
        url: String,
        request_id: u32,
    },
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
    SetDucking(u32),
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
}
