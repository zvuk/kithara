use kithara_bufpool::{ByteBuffer, HasPool};
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_stream::{AudioCodec, PrerollHint, ReaderChunkSignal, ReaderSeekSignal};

use crate::{
    Decoder, DecoderChunkOutcome, DecoderSeekOutcome,
    composed::DecoderRuntime,
    error::{DecodeError, DecodeResult},
    traits::BoxedSource,
    types::checked_audio_spec,
};

/// The APE library owns compressed-frame decoding. Retained PCM and emitted
/// chunks are admitted to the host pool before that frame is decoded.
pub(crate) struct ApeDecoder<S> {
    decoder: ape_decoder::ApeDecoder<BoxedSource>,
    runtime: DecoderRuntime<S>,
    spec: AudioSpec,
    duration: Duration,
    pcm: ByteBuffer,
    pcm_offset: usize,
    next_frame: u32,
    frame_offset: u64,
    seek_skip: usize,
    prepared: Option<DecodeResult<DecoderChunkOutcome>>,
}

impl<S> ApeDecoder<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    pub(crate) fn open(source: BoxedSource, runtime: DecoderRuntime<S>) -> DecodeResult<Self> {
        let decoder = ape_decoder::ApeDecoder::new(source).map_err(DecodeError::backend)?;
        let info = decoder.info();
        if info.bits_per_sample != 16 || info.is_big_endian || info.is_floating_point {
            return Err(DecodeError::UnsupportedCodec {
                codec: AudioCodec::Ape,
            });
        }
        let spec = checked_audio_spec(info.channels, info.sample_rate, "ape")?;
        let duration = spec.duration_for(info.total_samples)?;
        let pcm = runtime.pools.get::<u8>();
        Ok(Self {
            decoder,
            runtime,
            spec,
            duration,
            pcm,
            pcm_offset: 0,
            next_frame: 0,
            frame_offset: 0,
            seek_skip: 0,
            prepared: None,
        })
    }

    fn prepare_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        let channels = usize::from(self.spec.channels);
        let block_align = channels.checked_mul(2).ok_or(DecodeError::InvalidData {
            detail: "ape: block size overflow",
        })?;
        if self.pcm_offset == self.pcm.len() {
            if self.next_frame >= self.decoder.total_frames() {
                return Ok(DecoderChunkOutcome::Eof);
            }
            let blocks = usize::try_from(self.decoder.info().frame_samples(self.next_frame))?;
            let bytes = blocks
                .checked_mul(block_align)
                .ok_or(DecodeError::InvalidData {
                    detail: "ape: frame size overflow",
                })?;
            self.pcm.ensure_len(bytes)?;
            self.pcm.truncate(bytes);
            let decoded = self
                .decoder
                .decode_frame(self.next_frame)
                .map_err(DecodeError::backend)?;
            if decoded.len() != bytes {
                return Err(DecodeError::InvalidData {
                    detail: "ape: decoded frame length disagrees with header",
                });
            }
            self.pcm.copy_from_slice(&decoded);
            self.pcm_offset =
                self.seek_skip
                    .checked_mul(block_align)
                    .ok_or(DecodeError::InvalidData {
                        detail: "ape: seek offset overflow",
                    })?;
            self.seek_skip = 0;
            self.next_frame += 1;
        }
        let frames = ((self.pcm.len() - self.pcm_offset) / block_align).min(4_096);
        let samples = frames * channels;
        let mut output = self.runtime.pools.get_with_len::<f32>(samples)?;
        let bytes = &self.pcm[self.pcm_offset..self.pcm_offset + samples * 2];
        for (sample, pair) in output.iter_mut().zip(bytes.chunks_exact(2)) {
            *sample = f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32_768.0;
        }
        self.pcm_offset += samples * 2;
        let start = self.frame_offset;
        self.frame_offset += u64::try_from(frames)?;
        Ok(DecoderChunkOutcome::Chunk(Box::new(AudioChunk::new(
            AudioChunkInfo {
                spec: self.spec,
                timestamp: self.spec.duration_for(start)?,
                end_timestamp: self.spec.duration_for(self.frame_offset)?,
                frames: u32::try_from(frames)?,
                frame_offset: start,
                ..AudioChunkInfo::default()
            },
            output,
        ))))
    }
}

impl<S> Decoder for ApeDecoder<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    fn spec(&self) -> AudioSpec {
        self.spec
    }
    fn duration(&self) -> Option<Duration> {
        Some(self.duration)
    }
    fn update_byte_len(&self, _len: u64) {}

    fn prepare_next_chunk(&mut self) {
        if self.prepared.is_none() {
            self.prepared = Some(self.prepare_chunk());
        }
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        self.prepare_next_chunk();
        self.next_chunk_prepared()
    }

    fn next_chunk_prepared(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        let outcome = self.prepared.take().ok_or(DecodeError::InvalidData {
            detail: "ape: chunk not prepared",
        })??;
        if let Some(hooks) = self.runtime.hooks.as_mut() {
            hooks.on_chunk(match &outcome {
                DecoderChunkOutcome::Chunk(_) => ReaderChunkSignal::Chunk,
                DecoderChunkOutcome::Pending(reason) => ReaderChunkSignal::Pending(*reason),
                DecoderChunkOutcome::Eof => ReaderChunkSignal::Eof,
            });
        }
        Ok(outcome)
    }

    fn flush_reader_signals(&mut self) {
        if let Some(hooks) = self.runtime.hooks.as_mut() {
            hooks.flush();
        }
    }

    fn seek(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
        self.prepared = None;
        self.pcm.clear();
        self.pcm_offset = 0;
        let outcome = if position >= self.duration {
            self.next_frame = self.decoder.total_frames();
            DecoderSeekOutcome::PastEof {
                duration: self.duration,
            }
        } else {
            let frame = u64::try_from(self.spec.frames_for(position)?.get())?;
            let seek = self.decoder.seek(frame).map_err(DecodeError::backend)?;
            self.next_frame = seek.frame_index;
            self.seek_skip = usize::try_from(seek.skip_samples)?;
            self.frame_offset = seek.actual_sample;
            DecoderSeekOutcome::Landed {
                landed_at: self.spec.duration_for(seek.actual_sample)?,
                landed_frame: seek.actual_sample,
                landed_byte: Some(self.decoder.file_info().seek_byte(seek.frame_index)),
                preroll: PrerollHint::NotNeeded,
            }
        };
        if let Some(hooks) = self.runtime.hooks.as_mut() {
            hooks.on_seek(match outcome {
                DecoderSeekOutcome::Landed {
                    landed_byte,
                    preroll,
                    ..
                } => ReaderSeekSignal::Landed {
                    landed_byte,
                    preroll,
                },
                DecoderSeekOutcome::PastEof { .. } => ReaderSeekSignal::PastEof,
            });
        }
        Ok(outcome)
    }
}
