use std::sync::{
    Mutex, PoisonError,
    atomic::{AtomicU64, Ordering},
};

use kithara_events::{EventBus, QueueEvent, TrackId, TrackStatus};
use kithara_platform::CancelToken;
use kithara_play::ResourceConfig;

use crate::attempts::{AttemptGuard, Ticket};

/// Snapshot of a track entry in the queue.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TrackEntry {
    /// Source URL, if known. `None` for `TrackSource::Config` entries
    /// whose source is a pre-built [`ResourceConfig`] without a URL
    /// string (local file path, etc.).
    pub url: Option<String>,
    /// Display name derived from the URL or caller-supplied. May be empty.
    pub name: String,
    /// Stable identifier.
    pub id: TrackId,
    /// Current loading status.
    pub status: TrackStatus,
}

/// Input to [`Queue::append`](crate::Queue::append) /
/// [`Queue::insert`](crate::Queue::insert) describing how to load a track.
///
/// Two shapes:
/// - [`TrackSource::Uri`] — the queue builds a default
///   [`ResourceConfig`] from the [`QueueConfig`](crate::QueueConfig) templates
///   (`net`, `store`). Convenient for simple use.
/// - [`TrackSource::Config`] — the caller pre-builds a [`ResourceConfig`]
///   (useful for DRM keys, custom headers, format hints). The queue leaves
///   caller-set fields intact.
///
/// `TrackSource` is `Clone` so the queue can respawn a load when a
/// previously-consumed track is re-selected — re-tapping a track in
/// the playlist must work without the caller reconstructing anything.
#[derive(Clone)]
#[non_exhaustive]
pub enum TrackSource {
    /// Load from URL / path. Queue fills in defaults from `QueueConfig`.
    Uri(String),
    /// Caller-assembled resource config (DRM, headers, etc.). Boxed because
    /// [`ResourceConfig`] is ~100 bytes larger than the `Uri` variant.
    Config(Box<ResourceConfig>),
}

impl TrackSource {
    /// Returns the source URI if the variant is [`TrackSource::Uri`].
    #[must_use]
    pub fn uri(&self) -> Option<&str> {
        if let Self::Uri(s) = self {
            Some(s)
        } else {
            None
        }
    }
}

impl From<&str> for TrackSource {
    fn from(s: &str) -> Self {
        Self::Uri(s.to_string())
    }
}

impl From<String> for TrackSource {
    fn from(s: String) -> Self {
        Self::Uri(s)
    }
}

impl From<ResourceConfig> for TrackSource {
    fn from(c: ResourceConfig) -> Self {
        Self::Config(Box::new(c))
    }
}

impl From<Box<ResourceConfig>> for TrackSource {
    fn from(c: Box<ResourceConfig>) -> Self {
        Self::Config(c)
    }
}

/// Single owner of everything the queue knows about one track. Dropping the record aborts its
/// attempt via [`AttemptGuard`].
pub(crate) struct TrackRecord {
    pub(crate) id: TrackId,
    pub(crate) name: String,
    pub(crate) url: Option<String>,
    pub(crate) status: TrackStatus,
    pub(crate) source: TrackSource,
    pub(crate) attempt: Option<AttemptGuard>,
}

impl TrackRecord {
    pub(crate) fn new(id: TrackId, name: String, source: TrackSource) -> Self {
        Self {
            id,
            name,
            url: source.uri().map(str::to_string),
            status: TrackStatus::Pending,
            source,
            attempt: None,
        }
    }

    pub(crate) fn entry(&self) -> TrackEntry {
        TrackEntry {
            id: self.id,
            name: self.name.clone(),
            url: self.url.clone(),
            status: self.status.clone(),
        }
    }
}

/// Authoritative store for the queue's track list.
///
/// Single owner of `Vec<TrackRecord>`; shared between [`Queue`](crate::Queue)
/// and [`Loader`](crate::loader::Loader) via `Arc<Tracks>`. Every status
/// transition MUST go through [`Tracks::set_status`] (or the attempt ops
/// below) so the polled view and the reactive
/// [`QueueEvent::TrackStatusChanged`] stream never drift.
pub(crate) struct Tracks {
    bus: EventBus,
    inner: Mutex<Vec<TrackRecord>>,
    next_generation: AtomicU64,
}

impl Tracks {
    pub(crate) fn new(bus: EventBus) -> Self {
        Self {
            bus,
            inner: Mutex::new(Vec::new()),
            next_generation: AtomicU64::new(0),
        }
    }

    /// Lock the underlying `Vec<TrackRecord>` for direct read/write.
    /// Callers that only need to flip status should prefer
    /// [`Self::set_status`].
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Vec<TrackRecord>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Atomically mutate `record.status` and publish
    /// [`QueueEvent::TrackStatusChanged`]. `Cancelled` also aborts the
    /// track's live attempt — a cancelled track never keeps loading.
    /// No-op when `id` is not present (caller raced `Queue::remove`).
    pub(crate) fn set_status(&self, id: TrackId, status: TrackStatus) {
        let mut guard = self.lock();
        let Some(record) = guard.iter_mut().find(|r| r.id == id) else {
            return;
        };
        record.status = status.clone();
        let aborted = matches!(status, TrackStatus::Cancelled)
            .then(|| record.attempt.take())
            .flatten();
        drop(guard);
        drop(aborted);
        self.bus
            .publish(QueueEvent::TrackStatusChanged { id, status });
    }

    /// Original source for `id`, if still queued.
    pub(crate) fn source(&self, id: TrackId) -> Option<TrackSource> {
        self.lock()
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.source.clone())
    }

    /// Register a fresh attempt. Dedupes against a live attempt; replaces
    /// one that is already cancelled but still unwinding.
    pub(crate) fn begin_attempt(&self, id: TrackId, cancel: CancelToken) -> Option<Ticket> {
        let mut guard = self.lock();
        let ticket = match guard.iter_mut().find(|r| r.id == id) {
            Some(record)
                if record
                    .attempt
                    .as_ref()
                    .is_none_or(AttemptGuard::is_cancelled) =>
            {
                Some(install(record, &self.next_generation, cancel))
            }
            _ => None,
        };
        drop(guard);
        ticket
    }

    /// Move a track's pending load into the interactive lane: replace a
    /// still-waiting (or cancelled-but-unwinding) attempt. An attempt
    /// already holding a permit is kept - its download is progressing.
    /// Vacant means the attempt just finished; the completion path owns
    /// what happens next, so no new attempt starts.
    pub(crate) fn promote_attempt(&self, id: TrackId, cancel: CancelToken) -> Option<Ticket> {
        let mut guard = self.lock();
        let ticket = match guard.iter_mut().find(|r| r.id == id) {
            Some(record)
                if record
                    .attempt
                    .as_ref()
                    .is_some_and(|a| a.waiting || a.is_cancelled()) =>
            {
                Some(install(record, &self.next_generation, cancel))
            }
            _ => None,
        };
        drop(guard);
        ticket
    }

    /// Attempt won its lane permit: flip the track to `Loading`. `false`
    /// means the ticket was replaced or cancelled while waiting - the
    /// caller must release the permit and bail out without loading.
    pub(crate) fn mark_loading(&self, ticket: &Ticket) -> bool {
        let mut guard = self.lock();
        let claimed = guard
            .iter_mut()
            .find(|r| r.id == ticket.id)
            .is_some_and(|r| {
                let Some(attempt) = r.attempt.as_mut() else {
                    return false;
                };
                if attempt.generation != ticket.generation || attempt.is_cancelled() {
                    return false;
                }
                attempt.waiting = false;
                r.status = TrackStatus::Loading;
                true
            });
        drop(guard);
        if claimed {
            self.bus.publish(QueueEvent::TrackStatusChanged {
                id: ticket.id,
                status: TrackStatus::Loading,
            });
        }
        claimed
    }

    /// Attempt finished. Disarms and removes the guard this ticket owns
    /// (the token now belongs to the built `Resource`, or died with the
    /// dropped load future); `failure` flips the track to `Failed`.
    /// A stale ticket changes nothing.
    pub(crate) fn finish_attempt(&self, ticket: &Ticket, failure: Option<String>) {
        let mut guard = self.lock();
        let Some(record) = guard.iter_mut().find(|r| r.id == ticket.id) else {
            return;
        };
        if record
            .attempt
            .as_ref()
            .is_none_or(|a| a.generation != ticket.generation)
        {
            return;
        }
        if let Some(mut attempt) = record.attempt.take() {
            attempt.disarm();
        }
        let Some(reason) = failure else {
            return;
        };
        record.status = TrackStatus::Failed(reason.clone());
        drop(guard);
        self.bus.publish(QueueEvent::TrackStatusChanged {
            id: ticket.id,
            status: TrackStatus::Failed(reason),
        });
    }
}

fn install(record: &mut TrackRecord, generations: &AtomicU64, cancel: CancelToken) -> Ticket {
    let generation = generations.fetch_add(1, Ordering::Relaxed);
    // Replacing the guard drops the old one armed, cancelling the
    // superseded attempt.
    record.attempt = Some(AttemptGuard::new(generation, cancel));
    Ticket {
        id: record.id,
        generation,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case::from_str("https://example.com/song.mp3")]
    #[case::from_string("https://example.com/track.m3u8")]
    fn track_source_from_string_kind(#[case] url: &str) {
        let owned = url.to_string();
        let from_owned: TrackSource = owned.into();
        assert_eq!(from_owned.uri(), Some(url));
        let from_ref: TrackSource = url.into();
        assert_eq!(from_ref.uri(), Some(url));
    }

    #[kithara::test]
    fn track_source_from_resource_config() {
        let cfg =
            ResourceConfig::new("https://example.com/a.mp3").expect("BUG: hard-coded URL is valid");
        let src: TrackSource = cfg.into();
        assert!(matches!(src, TrackSource::Config(_)));
        assert_eq!(src.uri(), None);
    }

    fn tracks_with(id: TrackId) -> Tracks {
        let tracks = Tracks::new(EventBus::default());
        tracks.lock().push(TrackRecord::new(
            id,
            String::new(),
            "https://x/a.mp3".into(),
        ));
        tracks
    }

    fn token() -> CancelToken {
        CancelToken::never().child()
    }

    #[kithara::test]
    fn begin_dedupes_live_attempt() {
        let tracks = tracks_with(TrackId(1));
        assert!(tracks.begin_attempt(TrackId(1), token()).is_some());
        assert!(tracks.begin_attempt(TrackId(1), token()).is_none());
    }

    #[kithara::test]
    fn begin_replaces_cancelled_unwinding_attempt() {
        let tracks = tracks_with(TrackId(1));
        let first_cancel = token();
        let first = tracks
            .begin_attempt(TrackId(1), first_cancel.clone())
            .expect("BUG: vacant record must accept an attempt");
        tracks.set_status(TrackId(1), TrackStatus::Cancelled);
        assert!(first_cancel.is_cancelled(), "Cancelled must abort the load");
        let second = tracks
            .begin_attempt(TrackId(1), token())
            .expect("cancelled attempt must be replaceable");
        assert!(!tracks.mark_loading(&first), "replaced ticket loses claim");
        assert!(tracks.mark_loading(&second));
    }

    #[kithara::test]
    fn promote_replaces_waiting_and_cancels_it() {
        let tracks = tracks_with(TrackId(1));
        let parked_cancel = token();
        let parked = tracks
            .begin_attempt(TrackId(1), parked_cancel.clone())
            .expect("BUG: vacant record must accept an attempt");
        let promoted = tracks
            .promote_attempt(TrackId(1), token())
            .expect("waiting attempt must be promotable");
        assert!(parked_cancel.is_cancelled(), "parked attempt must abort");
        assert!(!tracks.mark_loading(&parked));
        assert!(tracks.mark_loading(&promoted));
    }

    #[kithara::test]
    fn promote_keeps_attempt_holding_permit() {
        let tracks = tracks_with(TrackId(1));
        let loading = tracks
            .begin_attempt(TrackId(1), token())
            .expect("BUG: vacant record must accept an attempt");
        assert!(tracks.mark_loading(&loading));
        assert!(
            tracks.promote_attempt(TrackId(1), token()).is_none(),
            "an attempt past the permit gate keeps its download"
        );
    }

    #[kithara::test]
    fn promote_vacant_is_noop() {
        let tracks = tracks_with(TrackId(1));
        assert!(tracks.promote_attempt(TrackId(1), token()).is_none());
    }

    #[kithara::test]
    fn finish_disarms_and_ignores_stale_ticket() {
        let tracks = tracks_with(TrackId(1));
        let first_cancel = token();
        let old = tracks
            .begin_attempt(TrackId(1), first_cancel.clone())
            .expect("BUG: vacant record must accept an attempt");
        let new = tracks
            .promote_attempt(TrackId(1), token())
            .expect("waiting attempt must be promotable");
        tracks.finish_attempt(&old, None);
        assert!(tracks.mark_loading(&new), "stale finish must not evict");
        tracks.finish_attempt(&new, None);
        assert!(
            tracks.begin_attempt(TrackId(1), token()).is_some(),
            "finished attempt must leave the record vacant"
        );
    }

    #[kithara::test]
    fn removing_record_cancels_attempt() {
        let tracks = tracks_with(TrackId(1));
        let cancel = token();
        let _ticket = tracks
            .begin_attempt(TrackId(1), cancel.clone())
            .expect("BUG: vacant record must accept an attempt");
        tracks.lock().clear();
        assert!(cancel.is_cancelled(), "dropping the record aborts the load");
    }

    #[kithara::test]
    fn finish_with_failure_sets_failed_once() {
        let tracks = tracks_with(TrackId(1));
        let ticket = tracks
            .begin_attempt(TrackId(1), token())
            .expect("BUG: vacant record must accept an attempt");
        tracks.finish_attempt(&ticket, Some("boom".into()));
        let status = tracks.lock()[0].status.clone();
        assert!(matches!(status, TrackStatus::Failed(_)));
    }
}
