use kithara_abr::AbrHandle;
use kithara_audio::ServiceClass;
use kithara_bufpool::PcmPool;
use kithara_events::EventBus;
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex, MutexGuard},
};
use tracing::debug;

use super::{
    super::platform::{activate_load, prepare_bound_load, restore_prepared_binding},
    PreparedBindingResource, PreparedBindingStamp, QueuedResource,
    playlist::{Playlist, QueuedLoad},
};
use crate::{
    api::{PlayerEvent, SlotId, TrackBinding},
    bridge::PlayerCmd,
    engine::{DeferredPlayerCmdError, EngineImpl},
    error::PlayError,
    resource::Resource,
    rt::{StreamShape, track::PlayerResource},
};

pub(crate) struct DispatchedLoad {
    pub(crate) abr_handle: Option<AbrHandle>,
    pub(crate) duration_seconds: f64,
    pub(crate) src: Arc<str>,
}

pub(in crate::player) struct BoundLoad {
    pub(in crate::player) player_resource: PlayerResource,
    pub(in crate::player) prepared_stamp: Option<PreparedBindingStamp>,
    pub(in crate::player) abr_handle: Option<AbrHandle>,
    pub(in crate::player) activation: Option<(CancelToken, PcmPool)>,
}

#[must_use = "load transactions must be dispatched"]
struct LoadTransaction<'a> {
    playlist: MutexGuard<'a, Playlist>,
    index: usize,
    binding: Option<TrackBinding>,
    item_id: Option<Arc<str>>,
    player_resource: PlayerResource,
    prepared_stamp: Option<PreparedBindingStamp>,
    abr_handle: Option<AbrHandle>,
    activation: Option<(CancelToken, PcmPool)>,
    duration_seconds: f64,
}

impl LoadTransaction<'_> {
    fn dispatch(self, engine: &EngineImpl, slot: SlotId) -> Result<DispatchedLoad, PlayError> {
        let Self {
            mut playlist,
            index,
            binding,
            item_id,
            player_resource,
            prepared_stamp,
            abr_handle,
            mut activation,
            duration_seconds,
        } = self;
        let src = Arc::clone(player_resource.src());
        let mut player_resource = Some(player_resource);
        let result = engine.try_send_slot_cmd_deferred(slot, || {
            activate_load(&mut player_resource, &mut activation)?;
            let resource = player_resource
                .take()
                .ok_or_else(|| PlayError::Internal("load transaction lost its resource".into()))?;
            Ok(PlayerCmd::LoadTrack {
                binding: binding.clone(),
                item_id: item_id.clone(),
                resource: Box::new(resource),
            })
        });
        match result {
            Ok(()) => {
                drop(playlist);
                Ok(DispatchedLoad {
                    abr_handle,
                    duration_seconds,
                    src,
                })
            }
            Err(DeferredPlayerCmdError::Unsent(error)) => {
                let resource = player_resource.ok_or_else(|| {
                    PlayError::Internal("unsent load consumed its queue resource".into())
                })?;
                restore_queue_resource(
                    &mut playlist,
                    index,
                    binding.is_some(),
                    resource,
                    prepared_stamp,
                )?;
                Err(error)
            }
            Err(DeferredPlayerCmdError::Rejected(rejected)) => {
                let error = rejected.error;
                let PlayerCmd::LoadTrack {
                    binding, resource, ..
                } = *rejected.command
                else {
                    return Err(PlayError::Internal(
                        "load dispatch rejected a non-load command".into(),
                    ));
                };
                restore_queue_resource(
                    &mut playlist,
                    index,
                    binding.is_some(),
                    *resource,
                    prepared_stamp,
                )?;
                Err(error)
            }
        }
    }
}

fn restore_queue_resource(
    playlist: &mut Playlist,
    index: usize,
    bound: bool,
    mut player_resource: PlayerResource,
    prepared_stamp: Option<PreparedBindingStamp>,
) -> Result<(), PlayError> {
    player_resource.set_service_class(ServiceClass::Idle);
    let (resource, renderer) = player_resource.release();
    let prepared = restore_prepared_binding(bound, renderer, prepared_stamp)?;
    restore_queued_resource(playlist, index, prepared, resource)
}

pub(in crate::player) fn restore_queued_resource(
    playlist: &mut Playlist,
    index: usize,
    prepared: Option<PreparedBindingResource>,
    resource: Resource,
) -> Result<(), PlayError> {
    if playlist.restore(index, prepared, resource) {
        Ok(())
    } else {
        Err(PlayError::Internal(
            "load transaction could not restore its queue resource".into(),
        ))
    }
}

pub(crate) struct ItemLoadContext<'a> {
    pub(crate) rate: f32,
    pub(crate) pitch_bend: f32,
    pub(crate) shape: StreamShape,
    pub(crate) pool: &'a PcmPool,
    pub(crate) stamp: PreparedBindingStamp,
    pub(crate) cancel: CancelToken,
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
        self.insert_queued(
            QueuedResource {
                binding: None,
                item_id,
                prepared: None,
                resource: Some(resource),
            },
            at_position,
        );
    }

    pub(in crate::player) fn insert_queued(
        &self,
        queued: QueuedResource,
        at_position: Option<usize>,
    ) {
        let (count, pos) = {
            let mut playlist = self.playlist.lock();
            let pos = playlist.insert(queued, at_position);
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
            playlist.replace(
                index,
                QueuedResource {
                    binding: None,
                    item_id,
                    prepared: None,
                    resource: Some(resource),
                },
            );
            drop(playlist);
            debug!(index, "item replaced");
        }
    }

    pub(crate) fn reserve_slots(&self, count: usize) {
        self.playlist.lock().reserve(count);
        debug!(count, "slots reserved");
    }

    pub(crate) fn dispatch_load(
        &self,
        index: usize,
        context: ItemLoadContext<'_>,
        engine: &EngineImpl,
        slot: SlotId,
    ) -> Result<Option<DispatchedLoad>, PlayError> {
        self.take_for_load(index, context)?
            .map(|transaction| transaction.dispatch(engine, slot))
            .transpose()
    }

    fn take_for_load(
        &self,
        index: usize,
        context: ItemLoadContext<'_>,
    ) -> Result<Option<LoadTransaction<'_>>, PlayError> {
        let mut playlist = self.playlist.lock();
        if index >= playlist.len() {
            return Ok(None);
        }

        let Some(queued) = playlist.take(index) else {
            return Ok(None);
        };
        let QueuedLoad {
            binding,
            item_id,
            prepared,
            resource,
        } = queued;
        let duration_seconds = resource
            .duration()
            .map_or(0.0, |duration| duration.as_secs_f64());
        let rate = context.rate;
        let pitch_bend = context.pitch_bend;
        let shape = context.shape;
        let BoundLoad {
            player_resource,
            prepared_stamp,
            abr_handle,
            activation,
        } = if binding.is_some() {
            prepare_bound_load(&mut playlist, index, resource, prepared, context)?
        } else {
            let ItemLoadContext {
                rate,
                pitch_bend,
                shape,
                pool,
                stamp: _,
                cancel,
            } = context;
            let abr_handle = resource.abr_handle();
            resource.set_playback_rate(rate);
            resource.set_transport_bend(pitch_bend);
            resource.set_host_sample_rate(shape.sample_rate);
            let src = Arc::clone(resource.src());
            drop(cancel);
            BoundLoad {
                player_resource: PlayerResource::new(resource, src, pool),
                prepared_stamp: None,
                abr_handle,
                activation: None,
            }
        };

        player_resource.set_playback_rate(rate);
        player_resource.set_transport_bend(pitch_bend);
        player_resource.set_host_sample_rate(shape.sample_rate);

        Ok(Some(LoadTransaction {
            playlist,
            index,
            binding,
            item_id,
            player_resource,
            prepared_stamp,
            abr_handle,
            activation,
            duration_seconds,
        }))
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

    pub(crate) fn has_binding(&self, index: usize) -> bool {
        self.playlist
            .lock()
            .get(index)
            .is_some_and(|item| item.binding.is_some())
    }

    pub(crate) fn current_has_binding(&self) -> bool {
        let playlist = self.playlist.lock();
        playlist
            .get(playlist.current())
            .is_some_and(|item| item.binding.is_some())
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

    use kithara_audio::{
        BeatGrid, PcmControl, PcmRead, PcmSession, ReadOutcome, SeekOutcome, TrackBeat,
        analysis::TrackAnalysis,
    };
    use kithara_bufpool::PcmPool;
    use kithara_decode::{DecodeError, PcmSpec, TrackMetadata};
    use kithara_events::{Envelope, Event, PlayerEvent};
    use kithara_platform::{CancelScope, time::Duration};

    use super::*;
    use crate::{
        api::{PlaybackDirection, SessionBeat, SlotId},
        engine::EngineConfig,
    };

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

    impl PcmSession for EofReader {
        fn duration(&self) -> Option<Duration> {
            None
        }

        fn event_bus(&self) -> &EventBus {
            &self.bus
        }

        fn metadata(&self) -> &TrackMetadata {
            &self.metadata
        }
    }

    impl PcmRead for EofReader {
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

        fn spec(&self) -> PcmSpec {
            self.spec
        }
    }

    impl PcmControl for EofReader {
        fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
            Ok(SeekOutcome::Landed {
                target: position,
                landed_at: position,
            })
        }
    }

    fn resource(src: &str) -> Resource {
        Resource::from_reader(EofReader::default(), Some(Arc::from(src)))
    }

    fn binding() -> TrackBinding {
        let sample_rate = NonZeroU32::new(44_100).expect("static rate");
        let analysis = TrackAnalysis::with_source_rate(
            Some(BeatGrid::new(
                120.0,
                vec![0, 22_050, 44_100],
                vec![0],
                Vec::new(),
            )),
            None,
            44_100,
            sample_rate,
        );
        TrackBinding::new(
            &analysis,
            sample_rate,
            SessionBeat::new(0.0).expect("finite session beat"),
            TrackBeat::new(0.0).expect("finite track beat"),
            PlaybackDirection::Forward,
        )
        .expect("valid binding")
    }

    fn load_context(pool: &PcmPool, cancel: CancelToken) -> ItemLoadContext<'_> {
        let shape = StreamShape {
            sample_rate: NonZeroU32::new(44_100).expect("static rate"),
            max_block_frames: NonZeroU32::new(512).expect("static block size"),
        };
        ItemLoadContext {
            rate: 1.0,
            pitch_bend: 1.0,
            shape,
            pool,
            stamp: PreparedBindingStamp::new(shape, 0),
            cancel,
        }
    }

    #[test]
    fn insert_and_remove_preserve_resource() {
        let queue = ItemQueue::new(EventBus::default());
        queue.insert(resource("first"), None, None);

        let removed = queue.remove_at(0).expect("inserted resource");

        assert_eq!(
            removed.resource.expect("inserted resource").src().as_ref(),
            "first"
        );
        assert_eq!(queue.item_count(), 0);
    }

    #[test]
    fn taking_a_bound_resource_retains_its_queue_coordinate_metadata() {
        let queue = ItemQueue::new(EventBus::default());
        queue.insert_queued(
            QueuedResource {
                binding: Some(binding()),
                item_id: None,
                prepared: None,
                resource: Some(resource("bound")),
            },
            None,
        );

        let taken = queue
            .playlist
            .lock()
            .take(0)
            .expect("bound resource is available");

        assert_eq!(taken.resource.src().as_ref(), "bound");
        assert!(!queue.has_resource(0));
        assert!(queue.current_has_binding());
    }

    #[test]
    fn load_transaction_serializes_queue_mutation_and_restores_rejection() {
        let queue = ItemQueue::new(EventBus::default());
        queue.insert(resource("original"), None, None);
        let pool = PcmPool::default();
        let scope = CancelScope::new(None);
        let transaction = queue
            .take_for_load(0, load_context(&pool, scope.token()))
            .expect("load preparation succeeds")
            .expect("original resource is available");

        assert!(queue.playlist.try_lock().is_err());
        let engine = EngineImpl::new(EngineConfig::default(), EventBus::default());
        let result = transaction.dispatch(&engine, SlotId::new(0));

        assert!(matches!(result, Err(PlayError::SlotNotFound(_))));
        assert!(queue.playlist.try_lock().is_ok());
        assert_eq!(
            queue
                .remove_at(0)
                .and_then(|item| item.resource)
                .expect("rejected resource is restored")
                .src()
                .as_ref(),
            "original"
        );
        engine.worker().shutdown();
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
            Ok(Envelope {
                event: Event::Player(PlayerEvent::CurrentItemChanged),
                ..
            })
        ));
        assert!(events.try_recv().is_err());
    }
}
