use kithara_bufpool::PcmPool;
use kithara_decode::{DecodeResult, PcmChunk, PcmSpec};
use kithara_platform::sync::Arc;
use kithara_stretch::{ElasticConfig, ElasticEngine, ElasticSpanConfig};

use super::{BoundError, BoundRenderer};
#[cfg(test)]
use crate::musical::{SessionBeat, SourceSchedule};
use crate::{
    tempo::{TempoSlot, slot::TempoBinding},
    traits::AudioEffect,
};

pub(crate) fn rate_supported(source_frames_per_output: f64) -> Option<bool> {
    Engine::rate_envelope()
        .ok()
        .map(|envelope| envelope.contains_rate(source_frames_per_output))
}

pub(crate) const fn render_span_frames() -> u64 {
    BoundRenderer::<Engine>::BLOCK_FRAMES
}

/// Numeric policy the bound slot plans under.
struct Consts;

impl Consts {
    /// Source-frame tolerance when comparing adjacent continuous spans.
    const CONTINUITY_TOLERANCE: f64 = 1.0e-6;
    /// Source-frame correction the plan may apply across one block.
    const MAX_CORRECTION_PER_BLOCK: f64 = 0.25;
    /// Source-frame phase error accepted at a block boundary before the plan
    /// refuses to continue.
    const MAX_PHASE_ERROR: f64 = 0.5;
    /// Headroom over the block's output frames for the source span. The rate
    /// envelope any engine declares is well inside 2x, so a doubled block
    /// covers the widest span a block can ask for.
    const SOURCE_HEADROOM: u64 = 2;
}

/// Builds the exact-span slot for a bound deck on the engine this build has.
///
/// # Errors
///
/// Returns [`BoundError`] when the engine cannot be prepared for this shape.
#[cfg(test)]
pub(crate) fn bound_slot(
    schedule: Arc<SourceSchedule>,
    session_origin: SessionBeat,
    spec: PcmSpec,
    pool: PcmPool,
) -> Result<Box<dyn AudioEffect>, BoundError> {
    let output_frames = usize::try_from(BoundRenderer::<Engine>::BLOCK_FRAMES)
        .map_err(|_| BoundError::BlockOverflow)?;
    let source_frames =
        usize::try_from(BoundRenderer::<Engine>::BLOCK_FRAMES * Consts::SOURCE_HEADROOM)
            .map_err(|_| BoundError::BlockOverflow)?;
    let config = ElasticConfig::try_from((
        spec.sample_rate.get(),
        usize::from(spec.channels.max(1)),
        source_frames,
        output_frames,
    ))?;
    let engine = Engine::prepare(config)?;
    let span_config = ElasticSpanConfig::try_from((
        Consts::CONTINUITY_TOLERANCE,
        Consts::MAX_PHASE_ERROR,
        Consts::MAX_CORRECTION_PER_BLOCK,
    ))?;
    Ok(Box::new(BoundRenderer::new(
        schedule,
        session_origin,
        engine,
        span_config,
        spec,
        pool,
    )?))
}

pub(crate) fn resident_slot(
    slot: TempoSlot,
    spec: PcmSpec,
    pool: PcmPool,
) -> Result<Box<dyn AudioEffect>, BoundError> {
    let output_frames = usize::try_from(BoundRenderer::<Engine>::BLOCK_FRAMES)
        .map_err(|_| BoundError::BlockOverflow)?;
    let source_frames =
        usize::try_from(BoundRenderer::<Engine>::BLOCK_FRAMES * Consts::SOURCE_HEADROOM)
            .map_err(|_| BoundError::BlockOverflow)?;
    let config = ElasticConfig::try_from((
        spec.sample_rate.get(),
        usize::from(spec.channels.max(1)),
        source_frames,
        output_frames,
    ))?;
    let engine = Engine::prepare(config)?;
    let span_config = ElasticSpanConfig::try_from((
        Consts::CONTINUITY_TOLERANCE,
        Consts::MAX_PHASE_ERROR,
        Consts::MAX_CORRECTION_PER_BLOCK,
    ))?;
    Ok(Box::new(TempoRenderer {
        renderer: BoundRenderer::resident(engine, span_config, spec, pool),
        active: None,
        slot,
    }))
}

struct TempoRenderer {
    slot: TempoSlot,
    renderer: BoundRenderer<Engine>,
    active: Option<Arc<TempoBinding>>,
}

impl TempoRenderer {
    fn sync_binding(&mut self) -> Result<bool, BoundError> {
        let target = self.slot.binding();
        let unchanged = self
            .active
            .as_ref()
            .zip(target.as_ref())
            .is_some_and(|(active, target)| Arc::ptr_eq(active, target));
        if unchanged {
            return Ok(true);
        }
        let Some(binding) = target else {
            if self.active.take().is_some() {
                self.renderer.deactivate();
            }
            return Ok(false);
        };
        self.renderer.bind_resident(Arc::clone(&binding))?;
        self.active = Some(binding);
        Ok(true)
    }
}

impl AudioEffect for TempoRenderer {
    fn flush(&mut self) -> Option<PcmChunk> {
        if self.active.is_some() {
            self.renderer.flush()
        } else {
            self.renderer.flush_streaming()
        }
    }

    fn held_source_frames(&self) -> u64 {
        if self.active.is_some() {
            self.renderer.held_source_frames()
        } else {
            self.renderer.held_streaming_source_frames()
        }
    }

    fn process(&mut self, chunk: PcmChunk) -> DecodeResult<Option<PcmChunk>> {
        let bound = self
            .sync_binding()
            .map_err(|error| kithara_decode::DecodeError::pcm_stream("tempo binding", error))?;
        if !bound {
            return self.renderer.process_streaming(chunk, self.slot.controls());
        }
        let rendered = self.renderer.process(chunk)?;
        if rendered.is_some()
            && let Some(binding) = &self.active
        {
            binding.mark_rendered();
        }
        Ok(rendered)
    }

    fn reset(&mut self) {
        self.renderer.reset();
        self.active = None;
    }
}

/// Signalsmith provides the priming contract required by bound rendering.
type Engine = kithara_stretch::SignalsmithElastic;

#[cfg(test)]
mod resident_tests {
    use std::num::NonZeroU32;

    use kithara_decode::PcmMeta;
    use kithara_events::PlaybackDirection;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        analysis::TrackAnalysis,
        musical::{SessionAnchor, SessionAnchorCell, SessionFrame, TrackBeatMap},
        tempo::{StretchControls, TempoState},
        waveform::BeatGrid,
    };

    struct Fixture;

    impl Fixture {
        const RATE: u32 = 48_000;
        const BEAT_FRAMES: u64 = 24_000;
        const CHUNK_FRAMES: usize = 2_048;
    }

    fn rate() -> NonZeroU32 {
        NonZeroU32::new(Fixture::RATE).expect("invariant: fixture rate is non-zero")
    }

    fn spec() -> PcmSpec {
        PcmSpec::new(2, rate())
    }

    fn schedule(origin_frame: u64, session_bpm: f64) -> Arc<SourceSchedule> {
        let markers: Vec<u64> = (0..16).map(|beat| beat * Fixture::BEAT_FRAMES).collect();
        let analysis = TrackAnalysis::with_source_rate(
            Some(BeatGrid::new(120.0, markers, vec![0], Vec::new())),
            None,
            Fixture::BEAT_FRAMES * 16,
            rate(),
        );
        let map = TrackBeatMap::new(&analysis, rate()).expect("fixture map is valid");
        let origin = map
            .track_beat_at(
                crate::musical::SourceFrame::try_from(origin_frame)
                    .expect("fixture source frame is exact"),
            )
            .expect("fixture origin is inside the map");
        let anchor = SessionAnchorCell::new();
        anchor.publish(
            SessionAnchor::new(
                SessionFrame::new(0),
                SessionBeat::default(),
                session_bpm / 60.0,
                rate(),
            )
            .expect("fixture grid is valid"),
        );
        Arc::new(SourceSchedule::new(
            map,
            origin,
            PlaybackDirection::Forward,
            anchor,
        ))
    }

    fn chunk(offset: u64) -> PcmChunk {
        let samples = vec![0.25; Fixture::CHUNK_FRAMES * 2];
        PcmChunk::new(
            PcmMeta {
                spec: spec(),
                frames: u32::try_from(Fixture::CHUNK_FRAMES).expect("fixture chunk fits u32"),
                frame_offset: offset,
                ..Default::default()
            },
            PcmPool::default().attach(samples),
        )
    }

    fn free_stage() -> (TempoSlot, Box<dyn AudioEffect>) {
        let slot = TempoSlot::from(StretchControls::new(1.0));
        let stage = resident_slot(slot.clone(), spec(), PcmPool::default())
            .expect("resident exact-span stage prepares");
        (slot, stage)
    }

    #[kithara::test]
    fn a_deck_bound_while_playing_keeps_its_decoder_and_its_effect_chain() {
        let (slot, mut stage) = free_stage();
        let stage_address = std::ptr::from_ref::<dyn AudioEffect>(&*stage) as *const ();
        let _ = stage.process(chunk(0)).expect("free chunk renders");

        slot.bind(
            schedule(Fixture::CHUNK_FRAMES as u64, 120.0),
            SessionBeat::default(),
        );
        let _ = stage
            .process(chunk(Fixture::CHUNK_FRAMES as u64))
            .expect("bound chunk renders through the resident stage");

        assert_eq!(
            stage_address,
            std::ptr::from_ref::<dyn AudioEffect>(&*stage) as *const ()
        );
    }

    #[kithara::test]
    fn a_deck_bound_while_playing_reaches_session_phase_without_a_seek() {
        let (slot, mut stage) = free_stage();
        let _ = stage.process(chunk(0)).expect("free chunk renders");
        slot.bind(
            schedule(Fixture::CHUNK_FRAMES as u64, 120.0),
            SessionBeat::default(),
        );
        assert_eq!(slot.state(), TempoState::Converging);

        let output = stage
            .process(chunk(Fixture::CHUNK_FRAMES as u64))
            .expect("the retained source tail primes the bound renderer");

        assert!(output.is_some());
        assert_eq!(slot.state(), TempoState::Bound);
    }

    #[kithara::test]
    fn an_unbind_while_playing_leaves_the_deck_running_free_at_the_tempo_it_had() {
        let (slot, mut stage) = free_stage();
        let _ = stage.process(chunk(0)).expect("free chunk renders");
        slot.bind(
            schedule(Fixture::CHUNK_FRAMES as u64, 144.0),
            SessionBeat::default(),
        );
        let _ = stage
            .process(chunk(Fixture::CHUNK_FRAMES as u64))
            .expect("bound chunk renders");

        let rate = slot.unbind();

        assert_eq!(slot.state(), TempoState::Free);
        assert!(rate.is_some());
        assert!((slot.controls().speed() - 1.2).abs() < 0.01);
    }
}
