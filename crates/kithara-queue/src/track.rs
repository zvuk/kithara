use std::sync::{Mutex, PoisonError};

use kithara_events::{EventBus, QueueEvent, TrackId, TrackStatus};
use kithara_play::ResourceConfig;

/// Snapshot of a track entry in the queue.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TrackEntry {
    /// Stable identifier.
    pub id: TrackId,
    /// Display name derived from the URL or caller-supplied. May be empty.
    pub name: String,
    /// Source URL, if known. `None` for `TrackSource::Config` entries
    /// whose source is a pre-built [`ResourceConfig`] without a URL
    /// string (local file path, etc.).
    pub url: Option<String>,
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
        match self {
            Self::Uri(s) => Some(s),
            Self::Config(_) => None,
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

/// Authoritative store for the queue's track list.
///
/// Single owner of `Vec<TrackEntry>`; shared between [`Queue`](crate::Queue)
/// and [`Loader`](crate::loader::Loader) via `Arc<Tracks>`. Every status
/// transition — `Loading` / `Slow` / `Failed` / `Loaded` / `Consumed`
/// / `Pending` respawn — MUST go through [`Tracks::set_status`] so that
/// the polled view (`tracks[i].status`) and the reactive
/// [`QueueEvent::TrackStatusChanged`] stream never drift.
pub(crate) struct Tracks {
    inner: Mutex<Vec<TrackEntry>>,
    bus: EventBus,
}

impl Tracks {
    pub(crate) fn new(bus: EventBus) -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
            bus,
        }
    }

    /// Lock the underlying `Vec<TrackEntry>` for direct read/write.
    /// Callers that only need to flip status should prefer
    /// [`Self::set_status`].
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Vec<TrackEntry>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Atomically mutate `entry.status` and publish
    /// [`QueueEvent::TrackStatusChanged`]. No-op when `id` is not
    /// present (caller most likely raced with `Queue::remove`).
    pub(crate) fn set_status(&self, id: TrackId, status: TrackStatus) {
        let mut guard = self.lock();
        if let Some(entry) = guard.iter_mut().find(|e| e.id == id) {
            entry.status = status.clone();
            drop(guard);
            self.bus
                .publish(QueueEvent::TrackStatusChanged { id, status });
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn track_source_from_str() {
        let src: TrackSource = "https://example.com/song.mp3".into();
        assert_eq!(src.uri(), Some("https://example.com/song.mp3"));
    }

    #[kithara::test]
    fn track_source_from_string() {
        let s = "https://example.com/track.m3u8".to_string();
        let src: TrackSource = s.into();
        assert_eq!(src.uri(), Some("https://example.com/track.m3u8"));
    }

    #[kithara::test]
    fn track_source_from_resource_config() {
        let cfg = ResourceConfig::new("https://example.com/a.mp3").expect("valid url");
        let src: TrackSource = cfg.into();
        assert!(matches!(src, TrackSource::Config(_)));
        assert_eq!(src.uri(), None);
    }
}
