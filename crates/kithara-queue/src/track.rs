use kithara_play::ResourceConfig;

/// Monotonic identifier for a track in the queue.
///
/// Stable across removals — if a track is removed and a new one appended, the
/// new track gets a fresh id. Use this to address tracks independently of
/// their index (which shifts as the queue is mutated).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub(crate) u64);

impl TrackId {
    /// Raw id value. Stable within a single [`Queue`](crate::Queue) instance.
    #[must_use]
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// Loading lifecycle of a track in the queue.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TrackStatus {
    /// Track is queued but loading has not started.
    Pending,
    /// Track is currently loading.
    Loading,
    /// Track has been loading longer than the soft timeout.
    Slow,
    /// Track is loaded and ready for playback.
    Loaded,
    /// Track load failed. The string carries a rendering of the underlying
    /// `PlayError` — the original typed error is delivered via
    /// [`QueueError::Play`](crate::QueueError::Play).
    Failed(String),
    /// Track was consumed by the engine after playback — needs a fresh load
    /// before [`Queue::select`](crate::Queue::select) can target it again.
    Consumed,
}

/// Snapshot of a track entry in the queue.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TrackEntry {
    /// Stable identifier.
    pub id: TrackId,
    /// Display name derived from the URL or caller-supplied. May be empty.
    pub name: String,
    /// Source URL, if known. `None` for local paths.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_source_from_str() {
        let src: TrackSource = "https://example.com/song.mp3".into();
        assert_eq!(src.uri(), Some("https://example.com/song.mp3"));
    }

    #[test]
    fn track_source_from_string() {
        let s = "https://example.com/track.m3u8".to_string();
        let src: TrackSource = s.into();
        assert_eq!(src.uri(), Some("https://example.com/track.m3u8"));
    }

    #[test]
    fn track_source_from_resource_config() {
        let cfg = ResourceConfig::new("https://example.com/a.mp3").expect("valid url");
        let src: TrackSource = cfg.into();
        assert!(matches!(src, TrackSource::Config(_)));
        assert_eq!(src.uri(), None);
    }

    #[test]
    fn track_status_roundtrip_simple_variants() {
        let s = TrackStatus::Loaded;
        assert_eq!(s.clone(), TrackStatus::Loaded);
        assert_ne!(s, TrackStatus::Pending);
    }

    #[test]
    fn track_status_failed_carries_reason() {
        let s = TrackStatus::Failed("timed out".to_string());
        match s {
            TrackStatus::Failed(reason) => assert_eq!(reason, "timed out"),
            _ => panic!("expected Failed"),
        }
    }
}
