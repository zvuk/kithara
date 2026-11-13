use kithara::{
    abr::AbrMode,
    prelude::{ResourceSrc, TrackMetadata},
    queue::{QueueError, Transition},
};
use kithara_app_library::Playable;

use crate::{
    config::AppConfig,
    pools::{AppQueueControl, AppTrackSource},
    sources::build_source,
};

#[must_use]
pub(crate) fn canonical_source(url: &str) -> String {
    ResourceSrc::parse(url).map_or_else(|_| url.to_string(), |src| src.to_string())
}

/// Put `track` on `queue` and make it current. A track already on this deck
/// is selected rather than appended, so loading twice is a no-op plus a select.
/// A new track carries its tags and cover URL in its resource configuration.
///
/// # Errors
/// Returns [`QueueError`] when the queue rejects the selection.
pub fn load_onto(
    queue: &AppQueueControl,
    track: &Playable,
    config: &AppConfig,
) -> Result<(), QueueError> {
    let source = canonical_source(&track.source);
    let existing = queue
        .tracks()
        .into_iter()
        .find(|queued| queued.url.as_deref() == Some(source.as_str()))
        .map(|queued| queued.id);
    let id = if let Some(id) = existing {
        id
    } else {
        let mode = if track.adaptive_streaming == Some(false) {
            AbrMode::manual(0)
        } else {
            AbrMode::default()
        };
        let mut built = build_source(&track.source, config, mode);
        if let AppTrackSource::Config(resource) = &mut built {
            resource.set_metadata(TrackMetadata {
                title: track.title.clone(),
                artist: track.artist.clone(),
                album: track.album.clone(),
                ..TrackMetadata::default()
            });
            if let Some(artwork) = &track.artwork {
                resource.set_artwork(ResourceSrc::Url(artwork.clone()));
            }
        }
        queue.append(built)?
    };
    let transition = if queue.is_playing() {
        Transition::Crossfade
    } else {
        Transition::None
    };
    queue.select(id, transition)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(native, flash(false))]
    fn a_source_is_normalized_like_queue_urls() {
        assert_eq!(
            canonical_source("HTTPS://Example.COM:443/a.mp3"),
            "https://example.com/a.mp3"
        );
        assert_eq!(canonical_source("/music/b.mp3"), "/music/b.mp3");
        assert_eq!(canonical_source("not a source"), "not a source");
    }
}
