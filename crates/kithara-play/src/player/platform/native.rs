use std::ops::Range;

use kithara_audio::ServiceClass;
use kithara_platform::{maybe_send::WasmSend, sync::Arc, traits::FromWithParams};
use kithara_stretch::{ElasticConfig, ElasticError, ElasticRateEnvelope, SignalsmithBackend};

use super::{
    super::{
        core::PlayerImpl,
        state::{
            items::{BoundLoad, ItemQueue, LoadTransaction, restore_queued_resource},
            playlist::{Playlist, QueuedResource},
        },
    },
    ItemLoadContext,
};
use crate::{
    api::{
        PlayerStatus, SessionBeat, SessionTransportSnapshot, Tempo, TrackBinding, TransportRevision,
    },
    bridge::TrackStart,
    error::PlayError,
    player::{
        node::StreamShape,
        track::{
            Active, BoundResource, ElasticPlanError, ElasticPrepareError, ElasticRenderOutcome,
            ElasticRenderer, PlayerResource, PlayerResourceKind, ReadOutcome, Ready,
            ReleasedPlayerResource, plan_elastic_segments,
        },
    },
    resource::Resource,
    session::{
        protocol::PreparationContext,
        render::{RenderContext, RenderFrame, SessionTransportCommit},
    },
};

pub(crate) type PreparedElasticRenderer = ElasticRenderer<Ready>;
pub(crate) type ActiveElasticRenderer = ElasticRenderer<Active>;

pub(crate) struct PreparedBindingResource {
    pub(crate) context: PreparationContext,
    pub(crate) renderer: PreparedElasticRenderer,
}

impl PlayerResource {
    pub(crate) fn begin_session_seek(
        &mut self,
        binding: &TrackBinding,
        target: SessionBeat,
        tempo: Tempo,
        revision: TransportRevision,
    ) -> Result<(), PlayError> {
        let PlayerResourceKind::Bound(bound) = &mut self.kind else {
            return Err(PlayError::SessionSeekRequiresBoundTrack);
        };
        bound
            .renderer
            .begin_relocation(binding, target, tempo, revision)
            .map_err(|error| PlayError::ElasticPreparation {
                reason: error.to_string(),
            })
    }

    pub(crate) fn cancel_session_seek(&mut self, revision: TransportRevision) {
        if let PlayerResourceKind::Bound(bound) = &mut self.kind {
            bound.renderer.discard_relocation(revision);
        }
    }

    pub(crate) fn new_bound(
        resource: Resource,
        src: Arc<str>,
        renderer: PreparedElasticRenderer,
    ) -> Self {
        Self {
            src,
            kind: BoundResource {
                renderer: renderer.activate(),
                resource: WasmSend::new(resource),
            }
            .into(),
        }
    }

    pub(crate) fn poll_session_seek(
        &mut self,
        revision: TransportRevision,
    ) -> Result<bool, PlayError> {
        let PlayerResourceKind::Bound(bound) = &mut self.kind else {
            return Err(PlayError::SessionSeekRequiresBoundTrack);
        };
        bound
            .renderer
            .poll_relocation(bound.resource.get_mut(), revision)
            .map_err(|error| PlayError::ElasticPreparation {
                reason: error.to_string(),
            })
    }

    pub(crate) fn read_elastic(
        &mut self,
        binding: &TrackBinding,
        context: &RenderContext,
        range: Range<usize>,
        output: &mut [&mut [f32]],
    ) -> ReadOutcome {
        let requested_frames = range.len();
        let PlayerResourceKind::Bound(bound) = &mut self.kind else {
            return ReadOutcome::Failed;
        };
        let outcome =
            bound
                .renderer
                .render(bound.resource.get_mut(), binding, context, range, output);
        match outcome {
            Ok(ElasticRenderOutcome::Ready { frames }) if frames == requested_frames => {
                ReadOutcome::Full { frames }
            }
            Ok(ElasticRenderOutcome::Eof) => ReadOutcome::Eof,
            Ok(ElasticRenderOutcome::Ready { .. }) | Err(_) => ReadOutcome::Failed,
        }
    }
}

impl BoundResource {
    pub(in crate::player) fn into_parts(self: Box<Self>) -> (Resource, PreparedElasticRenderer) {
        let Self { resource, renderer } = *self;
        (resource.into_inner(), renderer.into())
    }
}

impl PlayerImpl {
    /// Prepares and queues a stream-backed resource for session-bound elastic
    /// playback. Failure leaves the playlist unchanged.
    pub async fn insert_with_binding(
        &self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        binding: TrackBinding,
        at_position: Option<usize>,
    ) -> Result<(), PlayError> {
        let (resource, prepared) = self.prepare_bound_resource(resource, &binding).await?;
        self.core
            .items
            .insert_with_binding(resource, item_id, binding, prepared, at_position);
        Ok(())
    }

    /// Prepare and add the first bound item so it becomes audible at an exact
    /// beat without mutating the shared session transport.
    pub(in crate::player) async fn join_track_at(
        &self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        binding: TrackBinding,
        target: SessionBeat,
    ) -> Result<(), PlayError> {
        if self.item_count() != 0 {
            return Err(PlayError::SessionJoinPlayerNotEmpty);
        }
        self.ensure_engine_started()?;
        let slot = self.ensure_slot()?;
        let before = self.core.engine.session_transport()?;
        ensure_start_target(before, target)?;
        let (resource, prepared) = self
            .prepare_bound_resource_at(resource, &binding, target)
            .await?;
        let current_context = self.core.engine.preparation_context()?;
        let current = self.core.engine.session_transport()?;
        let context = prepared.context;
        validate_join_context(before, current, context, current_context, target)?;
        let dispatched = {
            let transaction = prepare_first_load(
                &self.core.items,
                QueuedResource {
                    binding: Some(binding),
                    item_id,
                    prepared: Some(prepared),
                    resource: Some(resource),
                },
                ItemLoadContext::build(context, self.core.item_load_params()),
            )?;
            transaction.dispatch(
                &self.core.engine,
                slot,
                TrackStart::Session {
                    target,
                    revision: current.revision(),
                },
            )?
        };
        self.phase.lock().set_abr_handle(dispatched.abr_handle);
        self.publish_current_track_snapshot(dispatched.duration_seconds);
        self.enter_playing();
        self.set_status(PlayerStatus::ReadyToPlay);
        self.announce_current_item(0);
        Ok(())
    }

    pub(in crate::player) fn validate_session_seek(
        &self,
        target: SessionBeat,
        tempo: Tempo,
        revision: TransportRevision,
        shape: StreamShape,
        binding: Option<&TrackBinding>,
    ) -> Result<(), PlayError> {
        let binding = binding.ok_or(PlayError::SessionSeekRequiresBoundTrack)?;
        let playback = self.playback_snapshot().ok_or(PlayError::NotReady)?;
        if playback.has_multiple_tracks() {
            return Err(PlayError::SessionSeekHandoverActive);
        }
        let context = tempo_context(target, tempo, revision, shape)?;
        required_source_frame(
            binding,
            &context,
            shape,
            SignalsmithBackend::<ElasticConfig>::rate_envelope(),
        )?;
        Ok(())
    }

    pub(in crate::player) fn validate_session_tempo(
        &self,
        snapshot: SessionTransportSnapshot,
        tempo: Tempo,
        shape: StreamShape,
        binding: Option<&TrackBinding>,
    ) -> Result<(), PlayError> {
        let Some(binding) = binding else {
            return Ok(());
        };
        let playback = self.playback_snapshot().ok_or(PlayError::NotReady)?;
        if playback.has_multiple_tracks() {
            return Err(PlayError::SessionTempoHandoverActive);
        }
        let available = playback.frontier() * f64::from(shape.sample_rate.get());
        validate_tempo_change(binding, snapshot, tempo, shape, available)
    }

    pub(in crate::player) fn validate_successor_tempo(
        playlist: &Playlist,
        tempo: Tempo,
        shape: StreamShape,
    ) -> Result<(), PlayError> {
        for index in playlist.current().saturating_add(1)..playlist.len() {
            let Some(queued) = playlist.get(index) else {
                continue;
            };
            let Some(binding) = queued.binding.as_ref() else {
                continue;
            };
            let prepared = queued
                .prepared
                .as_ref()
                .ok_or(PlayError::BindingPreparationRequired { index })?;
            if prepared.context.shape() != shape {
                return Err(PlayError::BindingPreparationStale { index });
            }
            prepared
                .renderer
                .validate_retarget(binding, binding.session_anchor(), tempo)
                .map_err(map_successor_retarget_error)?;
        }
        Ok(())
    }
}

fn prepare_first_load<'a>(
    items: &'a ItemQueue,
    queued: QueuedResource,
    context: ItemLoadContext<'_>,
) -> Result<LoadTransaction<'a>, PlayError> {
    let mut playlist = items.lock_playlist();
    let rollback_len = playlist.len();
    if rollback_len != 0 {
        return Err(PlayError::SessionJoinPlayerNotEmpty);
    }
    let index = playlist.insert(queued, None);
    match ItemQueue::prepare_load(playlist, index, context, rollback_len) {
        Ok(Some(transaction)) => Ok(transaction),
        Ok(None) => Err(PlayError::Internal(
            "inserted first resource is unavailable".into(),
        )),
        Err((mut playlist, error)) => {
            let _ = playlist.remove_at(index);
            Err(error)
        }
    }
}

fn ensure_start_target(
    snapshot: SessionTransportSnapshot,
    target: SessionBeat,
) -> Result<(), PlayError> {
    if target > snapshot.position() {
        Ok(())
    } else {
        Err(PlayError::SessionStartTargetElapsed {
            target: target.get(),
            position: snapshot.position().get(),
        })
    }
}

fn validate_join_context(
    before: SessionTransportSnapshot,
    current: SessionTransportSnapshot,
    prepared: PreparationContext,
    current_context: PreparationContext,
    target: SessionBeat,
) -> Result<(), PlayError> {
    if prepared != current_context
        || current_context.transport_revision() != current.revision()
        || current_context.tempo() != current.tempo()
        || current.revision() != before.revision()
        || current.tempo() != before.tempo()
    {
        return Err(PlayError::BindingPreparationContextChanged);
    }
    ensure_start_target(current, target)
}

fn validate_tempo_change(
    binding: &TrackBinding,
    snapshot: SessionTransportSnapshot,
    tempo: Tempo,
    shape: StreamShape,
    available: f64,
) -> Result<(), PlayError> {
    let envelope = SignalsmithBackend::<ElasticConfig>::rate_envelope();
    let old_context = tempo_context(
        snapshot.position(),
        snapshot.tempo(),
        snapshot.revision(),
        shape,
    )?;
    let boundary = old_context
        .session_beats()
        .ok_or_else(|| PlayError::ElasticPreparation {
            reason: "tempo preparation has no old session beat range".into(),
        })?
        .end;
    let next_revision =
        snapshot
            .revision()
            .checked_next()
            .ok_or_else(|| PlayError::ElasticPreparation {
                reason: "tempo preparation revision is exhausted".into(),
            })?;
    let new_context = tempo_context(boundary, tempo, next_revision, shape)?;
    let required = required_source_frame(binding, &old_context, shape, envelope)?.max(
        required_source_frame(binding, &new_context, shape, envelope)?,
    );
    if required > available {
        return Err(PlayError::SessionTempoLookAheadUnavailable {
            required,
            available,
        });
    }
    Ok(())
}

fn tempo_context(
    start: SessionBeat,
    tempo: Tempo,
    revision: TransportRevision,
    shape: StreamShape,
) -> Result<RenderContext, PlayError> {
    let output_frames = i64::from(shape.max_block_frames.get());
    let beat_span = f64::from(shape.max_block_frames.get()) * tempo.beats_per_second()
        / f64::from(shape.sample_rate.get());
    let end = SessionBeat::new(start.get() + beat_span).map_err(|error| {
        PlayError::ElasticPreparation {
            reason: error.to_string(),
        }
    })?;
    RenderContext::new(
        RenderFrame::new(0)..RenderFrame::new(output_frames),
        shape.sample_rate,
        Some(start..end),
        Some(SessionTransportCommit::new(tempo, true, revision)),
    )
    .ok_or_else(|| PlayError::ElasticPreparation {
        reason: "tempo preparation render context is invalid".into(),
    })
}

fn required_source_frame(
    binding: &TrackBinding,
    context: &RenderContext,
    shape: StreamShape,
    envelope: ElasticRateEnvelope,
) -> Result<f64, PlayError> {
    let output_frames = usize::try_from(shape.max_block_frames.get()).map_err(|error| {
        PlayError::ElasticPreparation {
            reason: error.to_string(),
        }
    })?;
    let revision = context
        .transport_commit()
        .ok_or_else(|| PlayError::ElasticPreparation {
            reason: "tempo preparation has no transport commit".into(),
        })?
        .revision();
    let segments = plan_elastic_segments(binding, context, 0..output_frames, 1, revision, envelope)
        .map_err(|error| match error {
            ElasticPlanError::UnsupportedRate {
                rate,
                minimum,
                maximum,
            } => PlayError::SessionTempoUnsupported {
                rate,
                minimum,
                maximum,
            },
            error => PlayError::ElasticPreparation {
                reason: error.to_string(),
            },
        })?;
    segments
        .iter()
        .map(|segment| {
            let span = segment.request.span();
            span.source_start().max(span.source_end())
        })
        .reduce(f64::max)
        .ok_or_else(|| PlayError::ElasticPreparation {
            reason: "tempo preparation produced no source segments".into(),
        })
}

impl ItemQueue {
    pub(crate) fn insert_with_binding(
        &self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        binding: TrackBinding,
        prepared: PreparedBindingResource,
        at_position: Option<usize>,
    ) {
        self.insert_queued(
            QueuedResource {
                item_id,
                binding: Some(binding),
                prepared: Some(prepared),
                resource: Some(resource),
            },
            at_position,
        );
    }
}

pub(crate) fn prepare_bound_load(
    playlist: &mut Playlist,
    index: usize,
    resource: Resource,
    binding: &TrackBinding,
    prepared: Option<PreparedBindingResource>,
    context: ItemLoadContext<'_>,
) -> Result<BoundLoad, PlayError> {
    let Some(mut prepared) = prepared else {
        restore_queued_resource(playlist, index, None, resource)?;
        return Err(PlayError::BindingPreparationRequired { index });
    };
    let Some(load_context) = context.preparation() else {
        restore_queued_resource(playlist, index, Some(prepared), resource)?;
        return Err(PlayError::BindingPreparationContextChanged);
    };
    if prepared.context.shape() != load_context.shape() {
        restore_queued_resource(playlist, index, Some(prepared), resource)?;
        return Err(PlayError::BindingPreparationStale { index });
    }
    if prepared.context.transport_revision() != load_context.transport_revision()
        && let Err(error) = prepared.renderer.retarget(
            binding,
            binding.session_anchor(),
            load_context.tempo(),
            load_context.transport_revision(),
        )
    {
        restore_queued_resource(playlist, index, Some(prepared), resource)?;
        return Err(map_successor_retarget_error(error));
    }
    prepared.context = load_context;

    let PreparedBindingResource {
        renderer,
        context: prepared_context,
    } = prepared;
    let abr_handle = resource.abr_handle();
    prepare_resource(
        &resource,
        context.rate(),
        context.pitch_bend(),
        context.shape(),
    );
    resource.set_service_class(ServiceClass::Idle);
    let src = Arc::clone(resource.src());
    let player_resource = PlayerResource::new_bound(resource, src, renderer);
    Ok(BoundLoad {
        player_resource,
        abr_handle,
        preparation: Some(prepared_context),
    })
}

fn map_successor_retarget_error(error: ElasticPrepareError) -> PlayError {
    match error {
        ElasticPrepareError::Elastic(ElasticError::InvalidRate(rate)) => {
            let envelope = SignalsmithBackend::<ElasticConfig>::rate_envelope();
            PlayError::SessionTempoUnsupported {
                rate,
                minimum: envelope.min_source_frames_per_output(),
                maximum: envelope.max_source_frames_per_output(),
            }
        }
        error => PlayError::ElasticPreparation {
            reason: error.to_string(),
        },
    }
}

pub(crate) fn restore_prepared_binding(
    released: ReleasedPlayerResource,
    context: Option<PreparationContext>,
) -> Result<(Resource, Option<PreparedBindingResource>), PlayError> {
    match (released, context) {
        (ReleasedPlayerResource::Bound(bound), Some(context)) => {
            let (resource, renderer) = bound.into_parts();
            Ok((
                resource,
                Some(PreparedBindingResource { context, renderer }),
            ))
        }
        (ReleasedPlayerResource::Linear(resource), None) => Ok((resource, None)),
        _ => Err(PlayError::Internal(
            "rejected load returned inconsistent binding state".into(),
        )),
    }
}

fn prepare_resource(resource: &Resource, rate: f32, pitch_bend: f32, shape: StreamShape) {
    resource.set_playback_rate(rate);
    resource.set_transport_bend(pitch_bend);
    resource.set_host_sample_rate(shape.sample_rate);
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_audio::{BeatGrid, TrackBeat, analysis::TrackAnalysis};
    use kithara_bufpool::PcmPool;
    use kithara_events::EventBus;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        api::{PlaybackDirection, SlotId},
        engine::{EngineConfig, EngineImpl},
        player::platform::ItemLoadParams,
        session::protocol::SessionRosterRevision,
        test_support::empty_resource,
    };

    #[kithara::test]
    fn bound_resource_type_requires_an_active_renderer() {
        fn renderer(bound: &BoundResource) -> &ActiveElasticRenderer {
            &bound.renderer
        }

        let _: fn(&BoundResource) -> &ActiveElasticRenderer = renderer;
    }

    #[kithara::test]
    fn first_load_transaction_removes_inserted_item_on_rejection() {
        let items = ItemQueue::new(EventBus::default());
        let pool = PcmPool::default();
        let engine = EngineImpl::new(EngineConfig::default(), EventBus::default());
        let shape = StreamShape::new(
            NonZeroU32::new(44_100).expect("static sample rate"),
            NonZeroU32::new(512).expect("static block size"),
        );
        let result = prepare_first_load(
            &items,
            QueuedResource {
                binding: None,
                item_id: None,
                prepared: None,
                resource: Some(empty_resource("joining")),
            },
            ItemLoadContext::build(
                shape,
                ItemLoadParams {
                    pool: &pool,
                    pitch_bend: 1.0,
                    rate: 1.0,
                },
            ),
        )
        .expect("first load preparation succeeds")
        .dispatch(&engine, SlotId::new(0), TrackStart::Immediate);

        assert!(matches!(result, Err(PlayError::SlotNotFound(_))));
        assert_eq!(items.item_count(), 0);
        engine.worker().shutdown();
    }

    #[kithara::test]
    fn join_rejects_stream_shape_change_after_preparation() {
        let target = SessionBeat::new(8.0).expect("finite target");
        let tempo = Tempo::new(120.0).expect("valid tempo");
        let revision = TransportRevision::FIRST;
        let before = SessionTransportSnapshot::new(
            SessionBeat::new(4.0).expect("finite position"),
            true,
            tempo,
            revision,
        );
        let current = SessionTransportSnapshot::new(
            SessionBeat::new(5.0).expect("finite position"),
            true,
            tempo,
            revision,
        );
        let prepared = PreparationContext::new(
            StreamShape::new(
                NonZeroU32::new(44_100).expect("static sample rate"),
                NonZeroU32::new(512).expect("static block size"),
            ),
            tempo,
            revision,
            SessionRosterRevision::new_for_test(1),
        );
        let changed = PreparationContext::new(
            StreamShape::new(
                NonZeroU32::new(48_000).expect("static sample rate"),
                NonZeroU32::new(512).expect("static block size"),
            ),
            tempo,
            revision,
            SessionRosterRevision::new_for_test(1),
        );

        assert!(matches!(
            validate_join_context(before, current, prepared, changed, target),
            Err(PlayError::BindingPreparationContextChanged)
        ));
    }

    fn binding() -> TrackBinding {
        let sample_rate = NonZeroU32::new(44_100).expect("static sample rate");
        let frames_per_beat = 26_460;
        let analysis = TrackAnalysis::with_source_rate(
            Some(BeatGrid::new(
                100.0,
                (0..=6).map(|beat| beat * frames_per_beat).collect(),
                vec![0],
                Vec::new(),
            )),
            None,
            frames_per_beat * 6,
            sample_rate,
        );
        TrackBinding::new(
            &analysis,
            sample_rate,
            SessionBeat::new(0.0).expect("finite session anchor"),
            TrackBeat::new(0.0).expect("finite track anchor"),
            PlaybackDirection::Forward,
        )
        .expect("valid binding")
    }

    fn variable_binding() -> TrackBinding {
        let sample_rate = NonZeroU32::new(44_100).expect("static sample rate");
        let analysis = TrackAnalysis::with_source_rate(
            Some(BeatGrid::new(
                100.0,
                vec![0, 26_460, 70_560, 114_660],
                vec![0],
                Vec::new(),
            )),
            None,
            114_660,
            sample_rate,
        );
        TrackBinding::new(
            &analysis,
            sample_rate,
            SessionBeat::new(0.0).expect("finite session anchor"),
            TrackBeat::new(0.0).expect("finite track anchor"),
            PlaybackDirection::Forward,
        )
        .expect("valid variable binding")
    }

    #[kithara::test]
    fn tempo_preflight_rejects_insufficient_source_lookahead() {
        let sample_rate = NonZeroU32::new(44_100).expect("static sample rate");
        let shape = StreamShape::new(
            sample_rate,
            NonZeroU32::new(512).expect("static block size"),
        );
        let snapshot = SessionTransportSnapshot::new(
            SessionBeat::new(0.0).expect("finite position"),
            true,
            Tempo::new(120.0).expect("valid tempo"),
            TransportRevision::FIRST,
        );

        let error = validate_tempo_change(
            &binding(),
            snapshot,
            Tempo::new(100.0).expect("valid changed tempo"),
            shape,
            0.0,
        )
        .expect_err("missing lookahead rejects before commit");

        assert!(matches!(
            error,
            PlayError::SessionTempoLookAheadUnavailable { .. }
        ));
    }

    #[kithara::test]
    fn tempo_preflight_rejects_one_unsupported_marker_segment() {
        let sample_rate = NonZeroU32::new(44_100).expect("static sample rate");
        let shape = StreamShape::new(
            sample_rate,
            NonZeroU32::new(512).expect("static block size"),
        );
        let block_beats = 512.0 * 120.0 / (44_100.0 * 60.0);
        let snapshot = SessionTransportSnapshot::new(
            SessionBeat::new(0.98 - block_beats).expect("finite position"),
            true,
            Tempo::new(120.0).expect("valid tempo"),
            TransportRevision::FIRST,
        );

        let error = validate_tempo_change(
            &variable_binding(),
            snapshot,
            Tempo::new(120.0).expect("valid changed tempo"),
            shape,
            200_000.0,
        )
        .expect_err("one unsupported marker segment rejects the transaction");

        assert!(matches!(error, PlayError::SessionTempoUnsupported { .. }));
    }
}
