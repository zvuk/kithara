use std::num::NonZeroU32;

use kithara_bufpool::PcmPool;
use kithara_events::EventBus;
use kithara_platform::sync::{Arc, Mutex};
use tracing::debug;

use super::{QueuedResource, playlist::Playlist};
use crate::{api::PlayerEvent, resource::Resource, rt::track::PlayerResource};

pub(crate) struct TakenItem {
    pub(crate) item_id: Option<Arc<str>>,
    pub(crate) player_resource: PlayerResource,
    pub(crate) abr_handle: Option<kithara_abr::AbrHandle>,
    pub(crate) duration_seconds: f64,
}

pub(crate) struct ItemQueue {
    playlist: Mutex<Playlist>,
    bus: EventBus,
}

impl ItemQueue {
    pub(crate) fn new(bus: EventBus) -> Self {
        Self {
            playlist: Mutex::default(),
            bus,
        }
    }

    pub(crate) fn advance_to_next_item(&self) {
        let mut playlist = self.playlist.lock();
        if let Some(index) = playlist.advance() {
            drop(playlist);
            self.announce_current_item(index);
            debug!(new_index = index, "advanced to next item");
        }
    }

    /// Sole publisher of `CurrentItemChanged`: emits only when `index` differs
    /// from the last announced item, so a `play()` resume of the same item
    /// stays quiet.
    pub(crate) fn announce_current_item(&self, index: usize) {
        if self.playlist.lock().mark_announced(index) {
            self.bus.publish(PlayerEvent::CurrentItemChanged);
        }
    }

    pub(crate) fn clear_item(&self, index: usize) {
        let mut playlist = self.playlist.lock();
        if index < playlist.len() {
            playlist.clear_item(index);
            drop(playlist);
            debug!(index, "item cleared");
        }
    }

    pub(crate) fn insert(
        &self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        at_position: Option<usize>,
    ) {
        let (count, pos) = {
            let mut playlist = self.playlist.lock();
            let pos = playlist.insert(QueuedResource { item_id, resource }, at_position);
            (playlist.len(), pos)
        };
        debug!(count, pos, "item inserted");
    }

    pub(crate) fn remove_at(&self, index: usize) -> Option<QueuedResource> {
        let mut playlist = self.playlist.lock();
        let removed = playlist.remove_at(index);
        let remaining = playlist.len();
        drop(playlist);
        debug!(index, remaining, "item removed");
        removed
    }

    pub(crate) fn replace_item_tagged(
        &self,
        index: usize,
        resource: Resource,
        item_id: Option<Arc<str>>,
    ) {
        let mut playlist = self.playlist.lock();
        if index < playlist.len() {
            playlist.replace(index, QueuedResource { item_id, resource });
            drop(playlist);
            debug!(index, "item replaced");
        }
    }

    pub(crate) fn reserve_slots(&self, count: usize) {
        self.playlist.lock().reserve(count);
        debug!(count, "slots reserved");
    }

    pub(crate) fn take_for_load(
        &self,
        index: usize,
        rate: f32,
        host_sample_rate: u32,
        pool: &PcmPool,
    ) -> Option<TakenItem> {
        let mut playlist = self.playlist.lock();
        if index >= playlist.len() {
            return None;
        }

        let queued = playlist.take(index)?;
        let (item_id, resource) = (queued.item_id, queued.resource);
        let duration_seconds = resource
            .duration()
            .map_or(0.0, |duration| duration.as_secs_f64());
        let abr_handle = resource.abr_handle();
        resource.set_playback_rate(rate);
        if let Some(sample_rate) = NonZeroU32::new(host_sample_rate) {
            resource.set_host_sample_rate(sample_rate);
        }
        let src = Arc::clone(resource.src());
        let player_resource = PlayerResource::new(resource, Arc::clone(&src), pool);
        drop(playlist);

        Some(TakenItem {
            item_id,
            player_resource,
            abr_handle,
            duration_seconds,
        })
    }

    pub(crate) fn clear_all(&self) {
        self.playlist.lock().clear();
    }

    pub(crate) fn current_index(&self) -> usize {
        self.playlist.lock().current()
    }

    pub(crate) fn has_resource(&self, index: usize) -> bool {
        self.playlist.lock().has_resource(index)
    }

    pub(crate) fn is_announced(&self, index: usize) -> bool {
        self.playlist.lock().is_announced(index)
    }

    pub(crate) fn item_count(&self) -> usize {
        self.playlist.lock().len()
    }

    pub(crate) fn set_current(&self, index: usize) {
        self.playlist.lock().set_current(index);
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_audio::{PcmReader, ReadOutcome, SeekOutcome};
    use kithara_decode::{DecodeError, PcmSpec, TrackMetadata};
    use kithara_events::{Event, PlayerEvent};
    use kithara_platform::time::Duration;

    use super::*;

    struct EofReader {
        bus: EventBus,
        spec: PcmSpec,
        metadata: TrackMetadata,
    }

    impl Default for EofReader {
        fn default() -> Self {
            Self {
                bus: EventBus::default(),
                spec: PcmSpec::new(2, NonZeroU32::new(44_100).expect("static rate")),
                metadata: TrackMetadata::default(),
            }
        }
    }

    impl PcmReader for EofReader {
        fn duration(&self) -> Option<Duration> {
            None
        }

        fn event_bus(&self) -> &EventBus {
            &self.bus
        }

        fn metadata(&self) -> &TrackMetadata {
            &self.metadata
        }

        fn position(&self) -> Duration {
            Duration::ZERO
        }

        fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
            Ok(ReadOutcome::Eof {
                position: Duration::ZERO,
            })
        }

        fn read_planar<'a>(
            &mut self,
            _output: &'a mut [&'a mut [f32]],
        ) -> Result<ReadOutcome, DecodeError> {
            Ok(ReadOutcome::Eof {
                position: Duration::ZERO,
            })
        }

        fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
            Ok(SeekOutcome::Landed {
                target: position,
                landed_at: position,
            })
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }
    }

    fn resource(src: &str) -> Resource {
        Resource::from_reader(EofReader::default(), Some(Arc::from(src)))
    }

    #[test]
    fn insert_and_remove_preserve_resource() {
        let queue = ItemQueue::new(EventBus::default());
        queue.insert(resource("first"), None, None);

        let removed = queue.remove_at(0).expect("inserted resource");

        assert_eq!(removed.resource.src().as_ref(), "first");
        assert_eq!(queue.item_count(), 0);
    }

    #[test]
    fn announce_deduplicates_current_item_event() {
        let bus = EventBus::default();
        let mut events = bus.subscribe();
        let queue = ItemQueue::new(bus);

        queue.announce_current_item(0);
        queue.announce_current_item(0);

        assert!(matches!(
            events.try_recv(),
            Ok(Event::Player(PlayerEvent::CurrentItemChanged))
        ));
        assert!(events.try_recv().is_err());
    }
}
