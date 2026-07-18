use kithara_audio::ServiceClass;
use kithara_bufpool::PcmPool;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_stretch::{ElasticRateEnvelope, SignalsmithElastic};

use super::super::{
    core::PlayerImpl,
    state::{
        items::{BoundLoad, ItemLoadContext, ItemQueue, restore_queued_resource},
        playlist::{Playlist, PreparedBindingStamp, QueuedResource},
    },
};
use crate::{
    api::{SessionBeat, SessionTransportSnapshot, Tempo, TrackBinding},
    error::PlayError,
    player::{
        node::StreamShape,
        track::{ElasticPlanError, PlayerResource, PreparedElasticRenderer, plan_elastic_segments},
    },
    resource::Resource,
    session::render::{RenderContext, RenderFrame, SessionTransportCommit},
};

pub(crate) struct PreparedBindingResource {
    pub(crate) renderer: PreparedElasticRenderer,
    pub(crate) stamp: PreparedBindingStamp,
}

impl PlayerImpl {
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

    pub(in crate::player) fn validate_session_seek(
        &self,
        target: SessionBeat,
        tempo: Tempo,
        revision: u64,
        shape: StreamShape,
        binding: Option<&TrackBinding>,
    ) -> Result<(), PlayError> {
        let Some(binding) = binding else {
            return Ok(());
        };
        let playback = self.playback_snapshot().ok_or(PlayError::NotReady)?;
        if playback.has_multiple_tracks() {
            return Err(PlayError::SessionSeekHandoverActive);
        }
        let context = tempo_context(target, tempo, revision, shape)?;
        required_source_frame(
            binding,
            &context,
            shape,
            SignalsmithElastic::rate_envelope(),
        )?;
        Ok(())
    }

    /// Prepares and queues a stream-backed resource for session-bound elastic
    /// playback. Failure leaves the playlist unchanged.
    pub async fn insert_with_binding(
        &self,
        resource: Resource,
        item_id: Option<Arc<str>>,
        binding: TrackBinding,
        at_position: Option<usize>,
    ) -> Result<(), PlayError> {
        let prepared = self.prepare_bound_resource(&resource, &binding).await?;
        self.core
            .items
            .insert_with_binding(resource, item_id, binding, prepared, at_position);
        Ok(())
    }
}

fn validate_tempo_change(
    binding: &TrackBinding,
    snapshot: SessionTransportSnapshot,
    tempo: Tempo,
    shape: StreamShape,
    available: f64,
) -> Result<(), PlayError> {
    let envelope = SignalsmithElastic::rate_envelope();
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
            .checked_add(1)
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
    revision: u64,
    shape: StreamShape,
) -> Result<RenderContext, PlayError> {
    let output_frames = i64::from(shape.max_block_frames.get());
    let beat_span = f64::from(shape.max_block_frames.get()) * tempo.beats_per_minute()
        / (f64::from(shape.sample_rate.get()) * 60.0);
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
            segment
                .request
                .source_start()
                .max(segment.request.source_end())
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
                binding: Some(binding),
                item_id,
                prepared: Some(prepared),
                resource: Some(resource),
            },
            at_position,
        );
    }
}

pub(crate) fn activate_load(
    player_resource: &mut Option<PlayerResource>,
    activation: &mut Option<(CancelToken, PcmPool)>,
) -> Result<(), PlayError> {
    let Some((cancel, pool)) = activation.take() else {
        return Ok(());
    };
    player_resource
        .as_mut()
        .ok_or_else(|| PlayError::Internal("load transaction lost its resource".into()))?
        .activate_prepared_elastic(cancel, pool)
        .map_err(|error| PlayError::ElasticPreparation {
            reason: error.to_string(),
        })
}

pub(crate) fn prepare_bound_load(
    playlist: &mut Playlist,
    index: usize,
    resource: Resource,
    prepared: Option<PreparedBindingResource>,
    context: ItemLoadContext<'_>,
) -> Result<BoundLoad, PlayError> {
    let Some(prepared) = prepared else {
        restore_queued_resource(playlist, index, None, resource)?;
        return Err(PlayError::BindingPreparationRequired { index });
    };
    if prepared.stamp != context.stamp {
        restore_queued_resource(playlist, index, Some(prepared), resource)?;
        return Err(PlayError::BindingPreparationStale { index });
    }

    let PreparedBindingResource {
        renderer,
        stamp: prepared_stamp,
    } = prepared;
    let abr_handle = renderer.abr_handle();
    prepare_resource(&resource, context.rate, context.pitch_bend, context.shape);
    resource.set_service_class(ServiceClass::Idle);
    let src = Arc::clone(resource.src());
    let mut player_resource = PlayerResource::new_elastic(resource, src);
    player_resource.install_prepared_elastic(renderer);
    Ok(BoundLoad {
        player_resource,
        prepared_stamp: Some(prepared_stamp),
        abr_handle,
        activation: Some((context.cancel, context.pool.clone())),
    })
}

pub(crate) fn restore_prepared_binding(
    bound: bool,
    renderer: Option<PreparedElasticRenderer>,
    stamp: Option<PreparedBindingStamp>,
) -> Result<Option<PreparedBindingResource>, PlayError> {
    match (bound, renderer, stamp) {
        (true, Some(renderer), Some(stamp)) => {
            Ok(Some(PreparedBindingResource { renderer, stamp }))
        }
        (false, None, None) => Ok(None),
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
    use kithara_test_utils::kithara;

    use super::*;
    use crate::api::PlaybackDirection;

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
            1,
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
            1,
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
