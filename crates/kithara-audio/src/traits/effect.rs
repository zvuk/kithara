use kithara_decode::{DecodeResult, PcmChunk, PcmMeta, PcmSpec};

use super::PresentationPoint;

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

/// Fixed-shape PCM visible to a frame-preserving effect.
///
/// Metadata and slice length are immutable, so an effect can observe source
/// provenance and mutate samples but cannot buffer, drop, resize, or retime a
/// presentation block.
#[non_exhaustive]
pub struct AudioBlockMut<'a> {
    meta: &'a PcmMeta,
    samples: &'a mut [f32],
}

impl<'a> AudioBlockMut<'a> {
    /// Borrow immutable PCM metadata and the matching fixed-length samples.
    #[must_use]
    pub fn new(meta: &'a PcmMeta, samples: &'a mut [f32]) -> Self {
        Self { meta, samples }
    }

    /// Immutable metadata for this presentation block.
    #[must_use]
    pub fn meta(&self) -> &PcmMeta {
        self.meta
    }

    /// Mutable interleaved samples with a fixed length.
    pub fn samples_mut(&mut self) -> &mut [f32] {
        self.samples
    }
}

/// Sample-domain effect that preserves block frames and metadata exactly.
#[kithara::mock(api = AudioEffectMock)]
pub trait AudioEffect: Send + 'static {
    /// Process one fixed-shape block in place.
    ///
    /// # Errors
    ///
    /// Returns a decode error when the samples cannot be processed. Effects
    /// cannot represent accumulation: the duration-changing stage has a
    /// separate private protocol.
    fn process(&mut self, block: AudioBlockMut<'_>) -> DecodeResult<()>;

    /// Clear state after a seek or decoder discontinuity.
    fn reset(&mut self);
}

/// One final-output reservation. It cannot be cloned or retained beyond the
/// render call that received it.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct OutputCredit<'a> {
    #[field(get, vis = "pub(crate)", copy)]
    channels: usize,
    #[field(get, vis = "pub(crate)", copy)]
    max_frames: usize,
    samples: &'a mut [f32],
}

impl<'a> OutputCredit<'a> {
    pub(crate) fn new(samples: &'a mut [f32], channels: usize, max_frames: usize) -> Self {
        Self {
            channels,
            max_frames,
            samples,
        }
    }

    pub(crate) fn samples_mut(&mut self) -> &mut [f32] {
        self.samples
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TempoStep {
    /// The latest controls or decoder shape have not been prepared off-RT yet.
    Preparing,
    /// Source advanced without output while the admitted chunk still has data.
    Consumed,
    /// The stage consumed its admitted chunk and needs another one.
    NeedSource,
    Rendered {
        frames: usize,
        meta: PcmMeta,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TempoBoundaryId(u64);

impl TempoBoundaryId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TempoPrepareRequest {
    Current { spec: PcmSpec },
    DecoderBoundary { spec: PcmSpec },
}

/// Proof that true source EOF was declared before finite DSP-tail draining.
pub(crate) struct TempoEofDebt {
    _private: (),
}

impl TempoEofDebt {
    pub(crate) const fn new() -> Self {
        Self { _private: () }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TempoEofStep {
    Drained,
    Rendered { frames: usize, meta: PcmMeta },
}

/// Proof that an ordered tempo boundary was reached after all older raw PCM,
/// before replacing the resident tempo engine.
pub(crate) struct TempoDiscontinuityDebt {
    _private: (),
}

impl TempoDiscontinuityDebt {
    pub(crate) const fn new() -> Self {
        Self { _private: () }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TempoDiscontinuityStep {
    Drained,
    Rendered { frames: usize, meta: PcmMeta },
}

/// The sole duration-changing presentation stage.
///
/// Steady rendering owns at most one admitted source chunk, consumes at most
/// 512 source frames per call, and writes at most the caller's output credit.
/// It cannot retain rendered output. True EOF and an ordered tempo boundary
/// are the only typed states that enable a finite caller-credited drain.
pub(crate) trait TempoStage: Send + 'static {
    /// Drop cores retired by the previous RT boundary in the unchecked shell.
    fn release_retired_off_rt(&mut self) {}

    /// Prepare the exact desired backend and control snapshot outside RT.
    fn service_off_rt(&mut self, request: TempoPrepareRequest) -> DecodeResult<()>;

    /// Boundary awaiting an ordered drain and allocation-free RT commit.
    fn prepared_boundary(&self) -> Option<TempoBoundaryId>;

    /// Move a previously prepared core into the active slot at an RT boundary.
    fn commit_prepared(&mut self, id: TempoBoundaryId) -> DecodeResult<()>;

    /// Number of admitted future-source chunks, always `0` or `1`.
    fn buffered_source_quanta(&self) -> usize;

    /// Current output PCM shape.
    fn output_spec(&self) -> PcmSpec;

    /// Admit one source chunk. Callers invoke this only when
    /// [`buffered_source_quanta`](Self::buffered_source_quanta) is zero.
    fn push_source(&mut self, chunk: PcmChunk) -> DecodeResult<()>;

    /// Render at most one caller-budgeted output block.
    fn render(
        &mut self,
        point: Option<PresentationPoint>,
        credit: OutputCredit<'_>,
        retire: &mut dyn FnMut(PcmChunk),
    ) -> DecodeResult<TempoStep>;

    /// Source frames admitted but not yet represented by rendered output.
    fn held_source_frames(&self) -> u64;

    /// Declare true source exhaustion and create one of the two legal finite
    /// rendered debts.
    fn finish_eof(&mut self) -> DecodeResult<TempoEofDebt>;

    /// Drain at most one credited EOF-tail block.
    fn render_eof(
        &mut self,
        debt: &mut TempoEofDebt,
        credit: OutputCredit<'_>,
        retire: &mut dyn FnMut(PcmChunk),
    ) -> DecodeResult<TempoEofStep>;

    /// Begin the bounded non-EOF drain at an ordered tempo boundary.
    fn begin_discontinuity(&mut self) -> DecodeResult<TempoDiscontinuityDebt>;

    /// Emit at most one credited block of old-engine source before reset.
    fn render_discontinuity(
        &mut self,
        debt: &mut TempoDiscontinuityDebt,
        credit: OutputCredit<'_>,
        retire: &mut dyn FnMut(PcmChunk),
    ) -> DecodeResult<TempoDiscontinuityStep>;

    /// Retire all owned state without resetting or dropping a backend on RT.
    fn deactivate(&mut self, retire: &mut dyn FnMut(PcmChunk)) -> DecodeResult<()>;
}
