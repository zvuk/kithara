use kithara_bufpool::PcmBuf;
use kithara_decode::{DecodeError, DecodeResult, PcmChunk, PcmMeta, PcmSpec};

use super::{
    PRESENTATION_FRAMES,
    coordinator::Presentation,
    output::{
        PresentResult, PresentedBlock, PresentedPcm, RecycleFailure, RecycleOutcome, split_meta,
        tempo_chunk,
    },
    state::RawItem,
};
use crate::{pipeline::fetch::Fetch, traits::AudioBlockMut};

impl Presentation {
    pub(crate) fn recycle_output(&mut self, chunk: PcmChunk) {
        if let RecycleOutcome::Rejected(failure) = self.buffers.recycle(chunk) {
            let error = self.defer_recycle_failure(failure);
            self.buffer_error = Some(error);
        }
    }

    fn defer_recycle_failure(&mut self, failure: RecycleFailure) -> DecodeError {
        let RecycleFailure { chunk, error } = failure;
        debug_assert!(self.rejected_output.is_none());
        self.rejected_output = Some(chunk);
        error
    }

    pub(super) fn commit(
        &mut self,
        output: PcmChunk,
        epoch: u64,
        held_source_frames: u64,
    ) -> DecodeResult<PresentResult> {
        if output.frames() > PRESENTATION_FRAMES {
            self.recycle_output(output);
            return Err(DecodeError::InvalidData {
                detail: "presentation stage exceeded its output credit",
            });
        }
        let metrics = PresentedBlock {
            frames: output.frames(),
            sample_rate: output.spec().sample_rate.get(),
        };
        let source_end = self.window.emitted(held_source_frames);
        let point = match self.publisher.prepare_commit(
            epoch,
            output.frames(),
            source_end.map(|source_end| (source_end.frame, source_end.rate)),
        ) {
            Ok(point) => point,
            Err(error) => {
                self.recycle_output(output);
                return Err(error);
            }
        };
        let fetch = Fetch::data(PresentedPcm::new(output, point), epoch);
        // The SPSC consumer can only free capacity. The preflight at the start
        // of this tick therefore reserves this one slot.
        if let Err(rejected) = self.output.try_push(fetch) {
            if let Fetch::Data { data, .. } = rejected {
                self.recycle_output(data.into_inner());
            }
            return Err(DecodeError::InvalidData {
                detail: "strict presentation reservation was lost",
            });
        }
        self.publisher.commit(point);
        self.final_committed = true;
        Ok(PresentResult::Produced(metrics))
    }

    pub(super) fn next_block<F>(&mut self, retire: &mut F) -> DecodeResult<Option<(PcmChunk, u64)>>
    where
        F: FnMut(PcmChunk),
    {
        let Some(RawItem::Data(raw)) = self.raw.front() else {
            return Err(DecodeError::InvalidData {
                detail: "presentation raw queue lost its data head",
            });
        };
        let channels = usize::from(raw.chunk.spec().channels);
        if channels == 0 {
            return Err(DecodeError::InvalidData {
                detail: "presentation received PCM with zero channels",
            });
        }
        let total_frames = raw.chunk.frames();
        let remaining =
            total_frames
                .checked_sub(raw.consumed_frames)
                .ok_or(DecodeError::InvalidData {
                    detail: "presentation consumed beyond the raw chunk",
                })?;
        let frames = remaining.min(PRESENTATION_FRAMES);
        if frames == 0 {
            let Some(RawItem::Data(raw)) = self.raw.pop_front() else {
                return Err(DecodeError::InvalidData {
                    detail: "presentation raw queue changed while retiring",
                });
            };
            retire(raw.chunk);
            return Err(DecodeError::InvalidData {
                detail: "presentation received an empty raw PCM chunk",
            });
        }
        let consumed_frames = raw.consumed_frames;
        let start_sample =
            consumed_frames
                .checked_mul(channels)
                .ok_or(DecodeError::InvalidData {
                    detail: "presentation sample offset overflow",
                })?;
        let samples = frames
            .checked_mul(channels)
            .ok_or(DecodeError::InvalidData {
                detail: "presentation block sample count overflow",
            })?;
        let end_sample = start_sample
            .checked_add(samples)
            .ok_or(DecodeError::InvalidData {
                detail: "presentation sample range overflow",
            })?;
        if raw.chunk.samples.get(start_sample..end_sample).is_none() {
            return Err(DecodeError::InvalidData {
                detail: "presentation PCM metadata exceeds its sample buffer",
            });
        }
        let next_consumed =
            consumed_frames
                .checked_add(frames)
                .ok_or(DecodeError::InvalidData {
                    detail: "presentation consumed-frame counter overflow",
                })?;
        let meta = split_meta(&raw.chunk.meta, consumed_frames, frames, total_frames)?;
        let epoch = raw.epoch;
        let spec = raw.chunk.spec();
        let Some(mut pcm) = self.buffers.take(spec)? else {
            return Ok(None);
        };
        let Some(RawItem::Data(raw)) = self.raw.front_mut() else {
            self.recycle_pcm(spec, pcm)?;
            return Err(DecodeError::InvalidData {
                detail: "presentation raw queue changed before slicing",
            });
        };
        let source = &raw.chunk.samples[start_sample..end_sample];
        pcm[..samples].copy_from_slice(source);
        pcm.truncate(samples);
        raw.consumed_frames = next_consumed;
        if next_consumed >= total_frames {
            let Some(RawItem::Data(raw)) = self.raw.pop_front() else {
                self.recycle_pcm(spec, pcm)?;
                return Err(DecodeError::InvalidData {
                    detail: "presentation raw queue changed after slicing",
                });
            };
            retire(raw.chunk);
        }
        Ok(Some((PcmChunk::new(meta, pcm), epoch)))
    }

    pub(super) fn output_buffer(&mut self, spec: PcmSpec) -> DecodeResult<Option<(PcmBuf, usize)>> {
        let channels = usize::from(spec.channels);
        if channels == 0 {
            return Err(DecodeError::InvalidData {
                detail: "tempo stage reported zero output channels",
            });
        }
        Ok(self.buffers.take(spec)?.map(|buffer| (buffer, channels)))
    }

    pub(super) fn process_effects(&mut self, mut chunk: PcmChunk) -> DecodeResult<PcmChunk> {
        let error = self.effects.iter_mut().find_map(|effect| {
            effect
                .process(AudioBlockMut::new(&chunk.meta, &mut chunk.samples))
                .err()
        });
        if let Some(error) = error {
            self.recycle_output(chunk);
            return Err(error);
        }
        Ok(chunk)
    }

    pub(super) fn recycle_pcm(&mut self, spec: PcmSpec, pcm: PcmBuf) -> DecodeResult<()> {
        let chunk = PcmChunk::new(
            PcmMeta {
                spec,
                ..Default::default()
            },
            pcm,
        );
        match self.buffers.recycle(chunk) {
            RecycleOutcome::Recycled => Ok(()),
            RecycleOutcome::Rejected(failure) => Err(self.defer_recycle_failure(failure)),
        }
    }

    pub(super) fn tempo_chunk(
        &mut self,
        pcm: PcmBuf,
        spec: PcmSpec,
        channels: usize,
        frames: usize,
        meta: PcmMeta,
    ) -> DecodeResult<PcmChunk> {
        match tempo_chunk(pcm, spec, channels, frames, meta) {
            Ok(chunk) => Ok(chunk),
            Err((error, pcm)) => {
                self.recycle_pcm(spec, pcm)?;
                Err(error)
            }
        }
    }

    pub(super) fn push_terminal(&mut self, marker: Fetch<PresentedPcm>) -> DecodeResult<()> {
        self.output
            .try_push(marker)
            .map_err(|_| DecodeError::InvalidData {
                detail: "strict presentation reservation was lost before terminal",
            })
    }
}
