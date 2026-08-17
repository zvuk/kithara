use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use kithara_bufpool::PcmPool;
use kithara_decode::{DecodeError, DecodeResult, PcmChunk, PcmMeta, PcmSpec, duration_for_frames};
use kithara_platform::sync::Arc;
use kithara_test_utils::kithara;

use super::*;
#[cfg(feature = "stretch-signalsmith")]
use crate::effects::timestretch::{StretchControls, StretchKind};
#[cfg(feature = "stretch-signalsmith")]
use crate::pipeline::config::create_presentation_chain;
use crate::{
    pipeline::{config::PresentationChain, fetch::Fetch},
    runtime::connect_strict,
    traits::{
        AudioBlockMut, AudioEffect, OutputCredit, PresentationPoint, TempoBoundaryId,
        TempoDiscontinuityDebt, TempoDiscontinuityStep, TempoEofDebt, TempoEofStep,
        TempoPrepareRequest, TempoStage, TempoStep,
    },
};

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

fn spec(sample_rate: u32) -> PcmSpec {
    PcmSpec::new(
        1,
        NonZeroU32::new(sample_rate).expect("test sample rate is non-zero"),
    )
}

fn chunk_with_spec(value: f32, frame_offset: u64, spec: PcmSpec) -> PcmChunk {
    let mut meta = PcmMeta::default();
    meta.spec = spec;
    meta.frames = u32::try_from(PRESENTATION_FRAMES).expect("test block fits u32");
    meta.frame_offset = frame_offset;
    meta.end_timestamp = duration_for_frames(
        meta.spec.sample_rate.get(),
        frame_offset + u64::try_from(PRESENTATION_FRAMES).expect("test block fits u64"),
    );
    PcmChunk::new(
        meta,
        PcmPool::default().attach(vec![value; PRESENTATION_FRAMES]),
    )
}

fn chunk(value: f32, frame_offset: u64) -> PcmChunk {
    chunk_with_spec(value, frame_offset, spec(44_100))
}

fn data(presentation: &mut Presentation, fetch: Fetch<PresentedPcm>) -> PcmChunk {
    let Fetch::Data { data, .. } = fetch else {
        panic!("expected PCM data");
    };
    let chunk: PcmChunk = data.into();
    let copy = PcmChunk::new(
        chunk.meta,
        PcmPool::default().attach(chunk.samples.to_vec()),
    );
    presentation.recycle_output(chunk);
    copy
}

fn step(presentation: &mut Presentation) -> PresentResult {
    presentation.service_off_rt();
    presentation
        .step(|_| {})
        .expect("presentation step must succeed")
}

fn admit(presentation: &mut Presentation, fetch: Fetch<PcmChunk>, context: &str) {
    assert!(presentation.admit(fetch).is_none(), "{context}");
}

#[kithara::rtsan_forbid_blocking]
fn guarded_step(
    presentation: &mut Presentation,
    retired: &mut Option<PcmChunk>,
) -> DecodeResult<PresentResult> {
    assert_no_alloc(|| {
        presentation.step(|chunk| {
            debug_assert!(retired.is_none());
            *retired = Some(chunk);
        })
    })
}

struct StampEffect {
    resets: Option<Arc<AtomicUsize>>,
    seen_frames: Arc<AtomicUsize>,
    value: f32,
}

impl AudioEffect for StampEffect {
    fn process(&mut self, mut block: AudioBlockMut<'_>) -> DecodeResult<()> {
        self.seen_frames.store(
            usize::try_from(block.meta().frames).expect("test frame count fits usize"),
            Ordering::Release,
        );
        block.samples_mut().fill(self.value);
        Ok(())
    }

    fn reset(&mut self) {
        if let Some(resets) = &self.resets {
            resets.fetch_add(1, Ordering::AcqRel);
        }
    }
}

struct SlowStage {
    control_boundary: Option<Arc<AtomicUsize>>,
    discontinuity_begins: Arc<AtomicUsize>,
    discontinuity_remaining: usize,
    eof_remaining: usize,
    held_frames: u64,
    max_credit: Arc<AtomicUsize>,
    output_frame: u64,
    prepared: Option<(TempoBoundaryId, PcmSpec)>,
    prepare_id: u64,
    reconfigures: Arc<AtomicUsize>,
    renders_left: usize,
    seen_presentation_output_end: Option<Arc<AtomicU64>>,
    source: Option<PcmChunk>,
    spec: PcmSpec,
}

impl SlowStage {
    fn new(max_credit: Arc<AtomicUsize>, eof_remaining: usize) -> Self {
        Self {
            control_boundary: None,
            discontinuity_begins: Arc::new(AtomicUsize::new(0)),
            discontinuity_remaining: 0,
            eof_remaining,
            held_frames: 0,
            max_credit,
            output_frame: 0,
            prepared: None,
            prepare_id: 0,
            reconfigures: Arc::new(AtomicUsize::new(0)),
            renders_left: 0,
            seen_presentation_output_end: None,
            source: None,
            spec: spec(44_100),
        }
    }

    fn with_seen_presentation_output_end(mut self, seen: Arc<AtomicU64>) -> Self {
        self.seen_presentation_output_end = Some(seen);
        self
    }

    fn with_control_boundary(mut self, requested: Arc<AtomicUsize>) -> Self {
        self.control_boundary = Some(requested);
        self
    }

    fn with_reconfigures(mut self, reconfigures: Arc<AtomicUsize>) -> Self {
        self.reconfigures = reconfigures;
        self
    }

    fn with_discontinuity(mut self, frames: usize, begins: Arc<AtomicUsize>) -> Self {
        self.discontinuity_begins = begins;
        self.discontinuity_remaining = frames;
        self
    }

    fn render_block(
        &mut self,
        mut credit: OutputCredit<'_>,
        frames: usize,
        value: f32,
    ) -> TempoStep {
        self.max_credit
            .fetch_max(credit.max_frames(), Ordering::AcqRel);
        credit.samples_mut()[..frames].fill(value);
        let mut meta = PcmMeta::default();
        meta.spec = self.spec;
        meta.frame_offset = self.output_frame;
        meta.frames = u32::try_from(frames).expect("test output fits u32");
        self.output_frame += u64::try_from(frames).expect("test output fits u64");
        TempoStep::Rendered { frames, meta }
    }
}

impl TempoStage for SlowStage {
    fn service_off_rt(&mut self, request: TempoPrepareRequest) -> DecodeResult<()> {
        match request {
            TempoPrepareRequest::Current { spec } => {
                if self.prepared.is_some() {
                    return Ok(());
                }
                if self
                    .control_boundary
                    .as_ref()
                    .is_some_and(|requested| requested.swap(0, Ordering::AcqRel) != 0)
                {
                    self.prepare_id = self.prepare_id.wrapping_add(1);
                    self.prepared = Some((TempoBoundaryId::new(self.prepare_id), spec));
                } else {
                    self.spec = spec;
                }
            }
            TempoPrepareRequest::DecoderBoundary { spec } => {
                if self.prepared.is_none() {
                    self.prepare_id = self.prepare_id.wrapping_add(1);
                    self.prepared = Some((TempoBoundaryId::new(self.prepare_id), spec));
                }
            }
        }
        Ok(())
    }

    fn prepared_boundary(&self) -> Option<TempoBoundaryId> {
        self.prepared.map(|(id, _)| id)
    }

    fn commit_prepared(&mut self, id: TempoBoundaryId) -> DecodeResult<()> {
        let Some((prepared, spec)) = self.prepared.take() else {
            return Err(DecodeError::InvalidData {
                detail: "test tempo stage has no prepared boundary",
            });
        };
        if prepared != id {
            return Err(DecodeError::InvalidData {
                detail: "test tempo stage received a stale boundary",
            });
        }
        self.reconfigures.fetch_add(1, Ordering::AcqRel);
        self.spec = spec;
        Ok(())
    }

    fn buffered_source_quanta(&self) -> usize {
        usize::from(self.source.is_some())
    }

    fn output_spec(&self) -> PcmSpec {
        self.spec
    }

    fn push_source(&mut self, chunk: PcmChunk) -> DecodeResult<()> {
        assert!(self.source.is_none());
        if chunk.spec() != self.spec {
            return Err(DecodeError::InvalidData {
                detail: "test tempo stage received source before spec adoption",
            });
        }
        self.source = Some(chunk);
        self.renders_left = 2;
        Ok(())
    }

    fn render(
        &mut self,
        point: Option<PresentationPoint>,
        credit: OutputCredit<'_>,
        retire: &mut dyn FnMut(PcmChunk),
    ) -> DecodeResult<TempoStep> {
        let Some(source) = self.source.as_ref() else {
            return Ok(TempoStep::NeedSource);
        };
        if let Some(seen) = &self.seen_presentation_output_end {
            seen.store(
                point.map_or(0, PresentationPoint::output_end),
                Ordering::Release,
            );
        }
        let value = source.samples[0];
        let frames = credit.max_frames();
        let step = self.render_block(credit, frames, value);
        self.renders_left -= 1;
        if self.renders_left == 0
            && let Some(source) = self.source.take()
        {
            retire(source);
            self.held_frames = u64::try_from(self.discontinuity_remaining)
                .expect("test discontinuity hold fits u64");
        }
        Ok(step)
    }

    fn held_source_frames(&self) -> u64 {
        let source = self.source.as_ref().map_or(0, |source| {
            u64::try_from(source.frames()).expect("test source frames fit u64")
        });
        source
            .checked_add(self.held_frames)
            .expect("test held source total fits u64")
    }

    fn finish_eof(&mut self) -> DecodeResult<TempoEofDebt> {
        Ok(TempoEofDebt::new())
    }

    fn render_eof(
        &mut self,
        _debt: &mut TempoEofDebt,
        mut credit: OutputCredit<'_>,
        _retire: &mut dyn FnMut(PcmChunk),
    ) -> DecodeResult<TempoEofStep> {
        if self.eof_remaining == 0 {
            return Ok(TempoEofStep::Drained);
        }
        self.max_credit
            .fetch_max(credit.max_frames(), Ordering::AcqRel);
        let frames = self.eof_remaining.min(credit.max_frames());
        credit.samples_mut()[..frames].fill(9.0);
        self.eof_remaining -= frames;
        let mut meta = PcmMeta::default();
        meta.spec = self.spec;
        meta.frame_offset = self.output_frame;
        meta.frames = u32::try_from(frames).expect("test EOF output fits u32");
        self.output_frame += u64::try_from(frames).expect("test EOF output fits u64");
        Ok(TempoEofStep::Rendered { frames, meta })
    }

    fn begin_discontinuity(&mut self) -> DecodeResult<TempoDiscontinuityDebt> {
        self.discontinuity_begins.fetch_add(1, Ordering::AcqRel);
        Ok(TempoDiscontinuityDebt::new())
    }

    fn render_discontinuity(
        &mut self,
        _debt: &mut TempoDiscontinuityDebt,
        mut credit: OutputCredit<'_>,
        _retire: &mut dyn FnMut(PcmChunk),
    ) -> DecodeResult<TempoDiscontinuityStep> {
        if self.discontinuity_remaining == 0 {
            self.held_frames = 0;
            return Ok(TempoDiscontinuityStep::Drained);
        }
        self.max_credit
            .fetch_max(credit.max_frames(), Ordering::AcqRel);
        let frames = self.discontinuity_remaining.min(credit.max_frames());
        credit.samples_mut()[..frames].fill(8.0);
        self.discontinuity_remaining -= frames;
        let mut meta = PcmMeta::default();
        meta.spec = self.spec;
        meta.frame_offset = self.output_frame;
        meta.frames = u32::try_from(frames).expect("test discontinuity output fits u32");
        self.output_frame += u64::try_from(frames).expect("test discontinuity output fits u64");
        Ok(TempoDiscontinuityStep::Rendered { frames, meta })
    }

    fn deactivate(&mut self, retire: &mut dyn FnMut(PcmChunk)) -> DecodeResult<()> {
        if let Some(source) = self.source.take() {
            retire(source);
        }
        self.renders_left = 0;
        self.held_frames = 0;
        self.discontinuity_remaining = 0;
        Ok(())
    }
}

fn identity_presentation(
    effects: Vec<Box<dyn AudioEffect>>,
) -> (Presentation, crate::runtime::Inlet<Fetch<PresentedPcm>>) {
    let (output, input) = connect_strict(1, None);
    let (publisher, _) = presentation_cell(0);
    (
        Presentation::new(
            4,
            PresentationChain::identity(effects),
            PcmPool::default(),
            spec(44_100),
            output,
            publisher,
            0,
        ),
        input,
    )
}

mod identity;
mod lifecycle;
mod recycle;
mod tempo;
