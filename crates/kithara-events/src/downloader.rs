#![forbid(unsafe_code)]

//! Events emitted by the unified downloader layer.
//!
//! All facts about HTTP fetches (lifecycle, cancellation, slow-load
//! signal) live here. Protocol crates (HLS, file) layer their own
//! semantics on top via their own events; they do NOT publish facts
//! about byte downloads themselves.
//!
//! Attribution to a track/peer is given by the **bus scope** the event
//! is published on — every peer registers a `scoped()` bus, and a
//! per-track subscriber sees only its scope's events. There is no
//! `peer_id`/`track_id` field in event payloads; if you need a
//! cross-track aggregate, subscribe to the root bus.
//!
//! Correlation across one fetch's lifecycle is via [`RequestId`]: the
//! same id appears in every event for one logical fetch (Enqueued →
//! Started → `LoadSlow` → Completed/Failed/Cancelled). The id is
//! allocated by the Downloader when it wraps an incoming `FetchCmd`
//! into its internal command — protocols do not need to know about
//! it; subscribers reconstruct the lifecycle by joining on `request_id`.
//! Subscribers can build their own `request_id → meaning` table by
//! reading the leading [`DownloaderEvent::RequestEnqueued`], which
//! carries the URL plus method and priority.

use std::num::NonZeroU64;

use kithara_net::NetError;
use kithara_platform::time::Duration;
use url::Url;

/// Stable id for a single Downloader request.
///
/// Allocated internally by the Downloader's `Registry` when wrapping a
/// `FetchCmd` into an `InternalCmd`. Echoed in every
/// [`DownloaderEvent`] for the same logical fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(NonZeroU64);

impl RequestId {
    /// Construct from a non-zero `u64`. Use a monotonic source (e.g.
    /// an `AtomicU64` started at 1).
    #[must_use]
    pub const fn new(id: NonZeroU64) -> Self {
        Self(id)
    }

    /// Get the inner `u64` for logging.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// HTTP method of a Downloader request.
///
/// Lives in `kithara-events` (not `kithara-stream`) because both the
/// command type and the lifecycle events refer to it; keeping it next
/// to the events avoids the dependency cycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RequestMethod {
    /// HTTP GET, streaming body. Default — used for large downloads
    /// (segments, files) that write directly to storage.
    #[default]
    Get,
    /// HTTP HEAD, headers only. Used for metadata queries
    /// (`Content-Length`).
    Head,
}

/// Effective scheduling priority of a request.
///
/// Used in the Downloader's 2×2 slot map (peer priority × cmd
/// priority): `High` commands and peers are processed before `Low`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RequestPriority {
    /// Latency-sensitive: demand segments, `execute`/`batch` calls,
    /// seek.
    High = 0,
    /// Background: prefetch, idle downloads. Default.
    #[default]
    Low = 1,
}

/// Why a fetch was cancelled.
///
/// Distinguishes the cancel paths so subscribers can tell e.g. a
/// seek-driven epoch flush from a peer drop or a downloader-wide
/// shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    /// The protocol's epoch cancel token fired (e.g. HLS bumped
    /// `seek_epoch`, invalidating in-flight fetches of the prior
    /// epoch).
    EpochCancel,
    /// The peer's own cancel token fired — the last `PeerHandle` clone
    /// was dropped, the protocol is shutting down its track.
    PeerCancel,
    /// Downloader-wide shutdown (the `Downloader` cancel token fired).
    DownloaderShutdown,
    /// The request's `CancelGroup` was already cancelled when the
    /// Downloader tried to spawn the fetch — the fetch never started.
    BeforeStart,
}

/// Events emitted by the unified downloader layer.
///
/// Published on the **peer's bus scope**, set via
/// `PeerHandle::with_bus`. A per-track subscriber sees only its own
/// fetches; a root-bus subscriber sees fetches from every peer.
///
/// Every variant for a single fetch carries the same [`RequestId`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DownloaderEvent {
    /// Request was accepted by the Downloader and placed into a
    /// priority slot. Published exactly once when `Registry::poll_peers`
    /// pushes the wrapped command into `slots[idx]`. Carries everything
    /// a subscriber needs to build a `request_id → meaning` table.
    RequestEnqueued {
        request_id: RequestId,
        url: Url,
        method: RequestMethod,
        priority: RequestPriority,
    },
    /// HTTP fetch started — slot acquired, task spawned. Between
    /// [`RequestEnqueued`](Self::RequestEnqueued) and this event there
    /// can be an arbitrary delay bounded by `max_concurrent` (slot
    /// pressure indicator: `wait_in_queue`).
    RequestStarted {
        request_id: RequestId,
        /// Time from `RequestEnqueued` to here.
        wait_in_queue: Duration,
    },
    /// `DownloaderConfig::soft_timeout` elapsed without the fetch
    /// completing. Informational; the request keeps running.
    LoadSlow {
        request_id: RequestId,
        elapsed: Duration,
    },
    /// HTTP body finished successfully.
    RequestCompleted {
        request_id: RequestId,
        bytes_transferred: u64,
        /// Total wall time from `RequestStarted` to here.
        duration: Duration,
        /// Pre-computed (`bytes / duration` → bps) so subscribers
        /// don't repeat the math.
        bandwidth_bps: u64,
    },
    /// HTTP fetch ended with a network-level error.
    RequestFailed {
        request_id: RequestId,
        error: NetError,
        /// `error.is_retryable()` — pre-evaluated.
        retryable: bool,
    },
    /// HTTP fetch was cancelled before completion.
    RequestCancelled {
        request_id: RequestId,
        reason: CancelReason,
        /// Bytes received before the cancel fired (if any).
        bytes_transferred: u64,
    },
    /// Effective priority of an in-queue (not-yet-started) request
    /// changed. Reserved shape — the Downloader does not emit this
    /// today (priority is immutable post-enqueue). Will be emitted
    /// when the scheduler learns to demote prefetch on demand arrival.
    PriorityChanged {
        request_id: RequestId,
        from: RequestPriority,
        to: RequestPriority,
    },
}
