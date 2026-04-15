use kithara_events::{TrackId, TrackStatus};
use kithara_play::ResourceConfig;

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
}
