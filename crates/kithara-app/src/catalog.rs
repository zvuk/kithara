use std::collections::BTreeMap;

use kithara::{
    events::TrackId,
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

/// Immutable catalogue descriptions of the tracks one deck has loaded.
#[derive(Default)]
pub struct Catalog {
    entries: BTreeMap<TrackId, Playable>,
}

impl Catalog {
    /// Loads a catalogue track, retaining an existing resource with the same identity.
    ///
    /// # Errors
    /// Returns invalid drag data or the queue's admission/selection refusal.
    pub fn load_onto(
        &mut self,
        queue: &AppQueueControl,
        track: &Playable,
        config: &AppConfig,
    ) -> Result<(), QueueError> {
        track
            .validate()
            .map_err(|error| QueueError::Resource(error.to_string()))?;
        let queued = queue.tracks();
        self.entries
            .retain(|id, _| queued.iter().any(|entry| entry.id == *id));
        let existing = track.identity.as_ref().map_or_else(
            || {
                let source = canonical_source(&track.source);
                queued
                    .iter()
                    .find(|entry| entry.url.as_deref() == Some(source.as_str()))
                    .map(|entry| entry.id)
            },
            |identity| {
                self.entries.iter().find_map(|(id, previous)| {
                    (previous.identity.as_ref() == Some(identity)).then_some(*id)
                })
            },
        );
        let id = if let Some(id) = existing {
            id
        } else {
            let id = queue.append(resource(track, &track.source, config))?;
            if !track.choices.is_empty() {
                self.entries.insert(id, track.clone());
            }
            id
        };
        let transition = if queue.is_playing() {
            Transition::Crossfade
        } else {
            Transition::None
        };
        queue.select(id, transition)
    }

    /// Selects an available source for the named current catalogue track.
    ///
    /// # Errors
    /// Returns a stale/unavailable choice or the queue's source-change refusal.
    pub fn select_quality(
        &self,
        queue: &AppQueueControl,
        id: TrackId,
        variant: Option<usize>,
        config: &AppConfig,
    ) -> Result<(), QueueError> {
        if queue
            .current()
            .is_none_or(|track| track.id != id || track.source_changing)
        {
            return Err(QueueError::NotReady(id));
        }
        let track = self.entries.get(&id).ok_or(QueueError::NotReady(id))?;
        let choice = variant
            .map_or_else(
                || track.choices.iter().find(|choice| choice.adaptive),
                |index| track.choices.get(index).filter(|choice| !choice.adaptive),
            )
            .ok_or(QueueError::NotReady(id))?;
        let current = queue
            .track_source(id)
            .ok_or(QueueError::UnknownTrackId(id))?;
        if current.uri() == Some(choice.source.as_str()) {
            return Ok(());
        }
        queue.replace_source(id, resource(track, choice.source.as_str(), config))
    }

    #[cfg(feature = "gui")]
    pub(crate) fn get(&self, id: TrackId) -> Option<&Playable> {
        self.entries.get(&id)
    }

    #[cfg(feature = "gui")]
    pub(crate) fn remove(&mut self, id: TrackId) {
        self.entries.remove(&id);
    }
}

fn resource(track: &Playable, url: &str, config: &AppConfig) -> AppTrackSource {
    let mut source = build_source(url, config);
    if let AppTrackSource::Config(resource) = &mut source {
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
    source
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
