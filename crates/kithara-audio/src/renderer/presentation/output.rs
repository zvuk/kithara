use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_decode::{DecodeError, DecodeResult, PcmChunk, PcmMeta, PcmSpec, duration_for_frames};

use super::coordinator::Presentation;
use crate::traits::PresentationPoint;

/// Fixed output quantum produced by the late presentation stage.
pub(crate) const PRESENTATION_FRAMES: usize = 512;

/// Two blocks can wait in the strict final ring while one is owned by the
/// consumer's current-chunk cursor.
pub(crate) const PRESENTATION_RING_BLOCKS: usize = 2;
const PRESENTATION_OUTPUT_BUFFERS: usize = PRESENTATION_RING_BLOCKS + 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PresentedBlock {
    pub(crate) frames: usize,
    pub(crate) sample_rate: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PresentResult {
    Advanced,
    Backpressured,
    Idle,
    Produced(PresentedBlock),
    Terminal,
}

#[derive(Debug, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct PresentedPcm {
    #[field(get, vis = "pub(crate)")]
    chunk: PcmChunk,
    #[field(get, vis = "pub(crate)", copy)]
    point: PresentationPoint,
}

impl PresentedPcm {
    pub(crate) fn new(chunk: PcmChunk, point: PresentationPoint) -> Self {
        Self { chunk, point }
    }

    pub(crate) fn returned(self) -> OutputDisposition {
        OutputDisposition::Returned(self.chunk)
    }

    pub(crate) fn detach(self) -> (PcmChunk, OutputDisposition) {
        let spec = self.chunk.spec();
        (self.chunk, OutputDisposition::Detached(spec))
    }

    pub(crate) fn into_inner(self) -> PcmChunk {
        self.chunk
    }
}

#[cfg(test)]
impl From<PresentedPcm> for PcmChunk {
    fn from(presented: PresentedPcm) -> Self {
        presented.into_inner()
    }
}

#[derive(Debug)]
pub(crate) enum OutputDisposition {
    Returned(PcmChunk),
    Detached(PcmSpec),
}

pub(super) struct OutputBuffers {
    free: Vec<PcmBuf>,
    pool: PcmPool,
    pub(super) spec: Option<PcmSpec>,
}

pub(super) struct RecycleFailure {
    pub(super) chunk: PcmChunk,
    pub(super) error: DecodeError,
}

#[must_use]
pub(super) enum RecycleOutcome {
    Recycled,
    Rejected(RecycleFailure),
}

pub(super) struct RestoreFailure {
    pub(super) disposition: OutputDisposition,
    pub(super) error: DecodeError,
}

#[must_use]
pub(super) enum RestoreOutcome {
    Restored,
    Rejected(RestoreFailure),
}

impl OutputBuffers {
    pub(super) fn new(pool: PcmPool) -> Self {
        Self {
            free: Vec::with_capacity(PRESENTATION_OUTPUT_BUFFERS),
            pool,
            spec: None,
        }
    }

    pub(super) fn prepare(&mut self, spec: PcmSpec) -> DecodeResult<bool> {
        if self.spec == Some(spec) {
            return Ok(true);
        }
        if self.spec.is_some() && self.free.len() != PRESENTATION_OUTPUT_BUFFERS {
            return Ok(false);
        }
        let samples = output_samples(spec)?;
        while self.free.len() < PRESENTATION_OUTPUT_BUFFERS {
            self.free.push(self.pool.get());
        }
        for buffer in &mut self.free {
            buffer
                .ensure_len(samples)
                .map_err(|error| DecodeError::pcm_stream("presentation buffer reserve", error))?;
            buffer.clear();
        }
        self.spec = Some(spec);
        Ok(true)
    }

    pub(super) fn take(&mut self, spec: PcmSpec) -> DecodeResult<Option<PcmBuf>> {
        if self.spec != Some(spec) {
            return Ok(None);
        }
        let Some(mut buffer) = self.free.pop() else {
            return Ok(None);
        };
        let samples = output_samples(spec)?;
        if buffer.capacity() < samples {
            self.free.push(buffer);
            return Err(DecodeError::InvalidData {
                detail: "presentation output reserve is smaller than its prepared PCM shape",
            });
        }
        buffer.clear();
        if buffer.ensure_len(samples).is_err() {
            self.free.push(buffer);
            return Err(DecodeError::InvalidData {
                detail: "presentation output reserve grew on the real-time path",
            });
        }
        Ok(Some(buffer))
    }

    pub(super) fn recycle(&mut self, chunk: PcmChunk) -> RecycleOutcome {
        if self.free.len() >= PRESENTATION_OUTPUT_BUFFERS {
            return RecycleOutcome::Rejected(RecycleFailure {
                chunk,
                error: DecodeError::InvalidData {
                    detail: "presentation output reserve received an extra buffer",
                },
            });
        }
        let spec = chunk.spec();
        if self.spec != Some(spec) {
            return RecycleOutcome::Rejected(RecycleFailure {
                chunk,
                error: DecodeError::InvalidData {
                    detail: "presentation output reserve received a stale PCM shape",
                },
            });
        }
        let samples = match output_samples(spec) {
            Ok(samples) => samples,
            Err(error) => return RecycleOutcome::Rejected(RecycleFailure { chunk, error }),
        };
        if chunk.samples.capacity() < samples {
            return RecycleOutcome::Rejected(RecycleFailure {
                chunk,
                error: DecodeError::InvalidData {
                    detail: "presentation output reserve received a short buffer",
                },
            });
        }
        let PcmChunk {
            samples: mut buffer,
            ..
        } = chunk;
        buffer.clear();
        self.free.push(buffer);
        RecycleOutcome::Recycled
    }

    pub(super) fn restore(&mut self, disposition: OutputDisposition) -> RestoreOutcome {
        match disposition {
            OutputDisposition::Returned(chunk) => match self.recycle(chunk) {
                RecycleOutcome::Recycled => RestoreOutcome::Restored,
                RecycleOutcome::Rejected(RecycleFailure { chunk, error }) => {
                    RestoreOutcome::Rejected(RestoreFailure {
                        disposition: OutputDisposition::Returned(chunk),
                        error,
                    })
                }
            },
            OutputDisposition::Detached(spec) => self.replace_detached(spec).map_or_else(
                |error| {
                    RestoreOutcome::Rejected(RestoreFailure {
                        disposition: OutputDisposition::Detached(spec),
                        error,
                    })
                },
                |()| RestoreOutcome::Restored,
            ),
        }
    }

    fn replace_detached(&mut self, spec: PcmSpec) -> DecodeResult<()> {
        if self.free.len() >= PRESENTATION_OUTPUT_BUFFERS {
            return Err(DecodeError::InvalidData {
                detail: "presentation output reserve received an extra detachment",
            });
        }
        if self.spec != Some(spec) {
            return Err(DecodeError::InvalidData {
                detail: "presentation output reserve received a stale detached PCM shape",
            });
        }
        let samples = output_samples(spec)?;
        let mut buffer = self.pool.get();
        buffer
            .ensure_len(samples)
            .map_err(|error| DecodeError::pcm_stream("detached presentation replacement", error))?;
        buffer.clear();
        self.free.push(buffer);
        Ok(())
    }
}

impl Presentation {
    pub(crate) fn restore_output(&mut self, disposition: OutputDisposition) {
        let RestoreOutcome::Rejected(failure) = self.buffers.restore(disposition) else {
            return;
        };
        let RestoreFailure { disposition, error } = failure;
        if let OutputDisposition::Returned(chunk) = disposition {
            debug_assert!(self.rejected_output.is_none());
            self.rejected_output = Some(chunk);
        }
        self.buffer_error = Some(error);
    }
}

pub(super) fn tempo_chunk(
    mut pcm: PcmBuf,
    spec: PcmSpec,
    channels: usize,
    frames: usize,
    mut meta: PcmMeta,
) -> Result<PcmChunk, (DecodeError, PcmBuf)> {
    if frames == 0 || frames > PRESENTATION_FRAMES {
        return Err((
            DecodeError::InvalidData {
                detail: "tempo stage violated its output frame credit",
            },
            pcm,
        ));
    }
    if meta.spec != spec {
        return Err((
            DecodeError::InvalidData {
                detail: "tempo stage output spec disagrees with its declared spec",
            },
            pcm,
        ));
    }
    let Some(samples) = frames.checked_mul(channels) else {
        return Err((
            DecodeError::InvalidData {
                detail: "tempo output sample count overflow",
            },
            pcm,
        ));
    };
    if samples > pcm.len() {
        return Err((
            DecodeError::InvalidData {
                detail: "tempo stage wrote beyond its output credit",
            },
            pcm,
        ));
    }
    pcm.truncate(samples);
    let Ok(meta_frames) = u32::try_from(frames) else {
        return Err((
            DecodeError::InvalidData {
                detail: "tempo output frame count exceeds u32",
            },
            pcm,
        ));
    };
    meta.frames = meta_frames;
    Ok(PcmChunk::new(meta, pcm))
}

fn output_samples(spec: PcmSpec) -> DecodeResult<usize> {
    let channels = usize::from(spec.channels);
    if channels == 0 {
        return Err(DecodeError::InvalidData {
            detail: "presentation output has zero channels",
        });
    }
    PRESENTATION_FRAMES
        .checked_mul(channels)
        .ok_or(DecodeError::InvalidData {
            detail: "presentation output sample capacity overflow",
        })
}

pub(super) fn split_meta(
    meta: &PcmMeta,
    offset: usize,
    frames: usize,
    total: usize,
) -> DecodeResult<PcmMeta> {
    let mut split = *meta;
    let rate = meta.spec.sample_rate.get();
    let offset_u64 = u64::try_from(offset).map_err(|_| DecodeError::InvalidData {
        detail: "presentation frame offset exceeds u64",
    })?;
    let frames_u64 = u64::try_from(frames).map_err(|_| DecodeError::InvalidData {
        detail: "presentation block length exceeds u64",
    })?;
    split.frame_offset =
        meta.frame_offset
            .checked_add(offset_u64)
            .ok_or(DecodeError::InvalidData {
                detail: "presentation frame offset overflow",
            })?;
    split.timestamp = meta
        .timestamp
        .checked_add(duration_for_frames(rate, offset_u64))
        .ok_or(DecodeError::InvalidData {
            detail: "presentation timestamp overflow",
        })?;
    split.end_timestamp = split
        .timestamp
        .checked_add(duration_for_frames(rate, frames_u64))
        .ok_or(DecodeError::InvalidData {
            detail: "presentation end timestamp overflow",
        })?;
    split.frames = u32::try_from(frames).map_err(|_| DecodeError::InvalidData {
        detail: "presentation block length exceeds u32",
    })?;
    if total > 0 && meta.source_bytes > 0 {
        let total_u128 = u128::try_from(total).map_err(|_| DecodeError::InvalidData {
            detail: "presentation source frame count exceeds u128",
        })?;
        let offset_u128 = u128::try_from(offset).map_err(|_| DecodeError::InvalidData {
            detail: "presentation source offset exceeds u128",
        })?;
        let source_bytes = u128::from(meta.source_bytes);
        let start = u64::try_from(source_bytes * offset_u128 / total_u128).map_err(|_| {
            DecodeError::InvalidData {
                detail: "presentation source byte offset exceeds u64",
            }
        })?;
        let end_offset = offset.checked_add(frames).ok_or(DecodeError::InvalidData {
            detail: "presentation source range overflow",
        })?;
        let end_offset_u128 = u128::try_from(end_offset).map_err(|_| DecodeError::InvalidData {
            detail: "presentation source end exceeds u128",
        })?;
        let end = u64::try_from(source_bytes * end_offset_u128 / total_u128).map_err(|_| {
            DecodeError::InvalidData {
                detail: "presentation source byte end exceeds u64",
            }
        })?;
        split.source_bytes = end.checked_sub(start).ok_or(DecodeError::InvalidData {
            detail: "presentation source byte range is reversed",
        })?;
        split.source_byte_offset = match meta.source_byte_offset {
            Some(source) => Some(source.checked_add(start).ok_or(DecodeError::InvalidData {
                detail: "presentation absolute source byte offset overflow",
            })?),
            None => None,
        };
    }
    Ok(split)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::*;

    fn test_spec() -> PcmSpec {
        PcmSpec::new(
            2,
            NonZeroU32::new(48_000).expect("test sample rate is non-zero"),
        )
    }

    fn output_chunk(spec: PcmSpec, samples: PcmBuf) -> PcmChunk {
        PcmChunk::new(
            PcmMeta {
                spec,
                frames: u32::try_from(PRESENTATION_FRAMES)
                    .expect("presentation frame count fits u32"),
                ..Default::default()
            },
            samples,
        )
    }

    #[kithara::test]
    fn mixed_returned_and_detached_outputs_restore_the_bounded_reserve() {
        let spec = test_spec();
        let mut buffers = OutputBuffers::new(PcmPool::default());
        assert!(
            buffers
                .prepare(spec)
                .expect("initial output reserve prepares")
        );
        let returned_first = buffers
            .take(spec)
            .expect("prepared reserve is valid")
            .expect("first resident buffer is available");
        let detached = buffers
            .take(spec)
            .expect("prepared reserve is valid")
            .expect("second resident buffer is available");
        let returned_last = buffers
            .take(spec)
            .expect("prepared reserve is valid")
            .expect("third resident buffer is available");
        assert!(
            buffers
                .prepare(spec)
                .expect("same-spec preparation remains valid")
        );
        assert!(
            buffers
                .take(spec)
                .expect("same-spec reserve remains valid")
                .is_none(),
            "same-spec preparation must not top up in-flight outputs"
        );

        assert!(matches!(
            buffers.restore(OutputDisposition::Returned(output_chunk(
                spec,
                returned_first
            ))),
            RestoreOutcome::Restored
        ));
        assert!(matches!(
            buffers.restore(OutputDisposition::Detached(spec)),
            RestoreOutcome::Restored
        ));
        assert!(matches!(
            buffers.restore(OutputDisposition::Returned(output_chunk(
                spec,
                returned_last
            ))),
            RestoreOutcome::Restored
        ));

        for _ in 0..PRESENTATION_OUTPUT_BUFFERS {
            assert!(
                buffers
                    .take(spec)
                    .expect("restored reserve is valid")
                    .is_some(),
                "each disposition must restore one resident output"
            );
        }
        assert!(
            buffers
                .take(spec)
                .expect("restored reserve remains valid")
                .is_none(),
            "the output reserve remains bounded"
        );
        drop(detached);
    }
}
