use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::PcmPool;
use kithara_resampler::{
    Resampler, ResamplerBackend, ResamplerBuildError, ResamplerCapabilities, ResamplerMode,
    ResamplerProcess, ResamplerSettings,
};

use super::wrap;
use crate::{
    DecodeResult, Decoder, DecoderChunkOutcome, DecoderResamplerConfig, DecoderSeekOutcome,
    PcmChunk, PcmMeta, PcmSpec,
};

#[derive(Clone)]
struct AdapterProbeBackend;

impl ResamplerBackend for AdapterProbeBackend {
    type Resampler = AdapterProbeResampler;

    fn build(&self, settings: &ResamplerSettings) -> Result<Self::Resampler, ResamplerBuildError> {
        Ok(AdapterProbeResampler {
            channels: settings.channels,
            mode: settings.mode,
        })
    }

    fn capabilities(&self) -> ResamplerCapabilities {
        ResamplerCapabilities::FIXED_RATIO | ResamplerCapabilities::STANDALONE
    }

    fn name(&self) -> &'static str {
        "adapter-probe"
    }
}

struct AdapterProbeResampler {
    channels: NonZeroUsize,
    mode: ResamplerMode,
}

impl Resampler for AdapterProbeResampler {
    fn capabilities(&self) -> ResamplerCapabilities {
        ResamplerCapabilities::FIXED_RATIO | ResamplerCapabilities::STANDALONE
    }

    fn channels(&self) -> NonZeroUsize {
        self.channels
    }

    fn input_frames_max(&self) -> usize {
        4
    }

    fn input_frames_next(&self) -> usize {
        4
    }

    fn mode(&self) -> ResamplerMode {
        self.mode
    }

    fn output_frames_for_input(&self, input_frames: usize) -> usize {
        input_frames
    }

    fn output_frames_max(&self) -> usize {
        4
    }

    fn output_frames_next(&self) -> usize {
        4
    }

    fn process_into_buffer(
        &mut self,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
    ) -> Result<ResamplerProcess, kithara_resampler::ResamplerError> {
        for (dst, src) in output.iter_mut().zip(input) {
            dst[..src.len()].copy_from_slice(src);
        }
        Ok(ResamplerProcess::new(4, 4))
    }

    fn reset(&mut self) {}
}

#[derive(Clone)]
struct DelayedProbeBackend;

impl ResamplerBackend for DelayedProbeBackend {
    type Resampler = DelayedProbeResampler;

    fn build(&self, settings: &ResamplerSettings) -> Result<Self::Resampler, ResamplerBuildError> {
        Ok(DelayedProbeResampler {
            channels: settings.channels,
            has_pending: false,
            mode: settings.mode,
            pending: vec![vec![0.0; 4]; settings.channels.get()],
        })
    }

    fn capabilities(&self) -> ResamplerCapabilities {
        ResamplerCapabilities::FIXED_RATIO | ResamplerCapabilities::STANDALONE
    }

    fn name(&self) -> &'static str {
        "delayed-probe"
    }
}

struct DelayedProbeResampler {
    channels: NonZeroUsize,
    has_pending: bool,
    mode: ResamplerMode,
    pending: Vec<Vec<f32>>,
}

impl Resampler for DelayedProbeResampler {
    fn capabilities(&self) -> ResamplerCapabilities {
        ResamplerCapabilities::FIXED_RATIO | ResamplerCapabilities::STANDALONE
    }

    fn channels(&self) -> NonZeroUsize {
        self.channels
    }

    fn input_frames_max(&self) -> usize {
        4
    }

    fn input_frames_next(&self) -> usize {
        4
    }

    fn mode(&self) -> ResamplerMode {
        self.mode
    }

    fn output_delay(&self) -> usize {
        4
    }

    fn output_frames_for_input(&self, input_frames: usize) -> usize {
        input_frames
    }

    fn output_frames_max(&self) -> usize {
        4
    }

    fn output_frames_next(&self) -> usize {
        4
    }

    fn process_into_buffer(
        &mut self,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
    ) -> Result<ResamplerProcess, kithara_resampler::ResamplerError> {
        for (channel, dst) in output.iter_mut().enumerate() {
            if self.has_pending {
                dst[..4].copy_from_slice(&self.pending[channel]);
            } else {
                dst[..4].fill(0.0);
            }
            self.pending[channel].copy_from_slice(&input[channel][..4]);
        }
        self.has_pending = true;
        Ok(ResamplerProcess::new(4, 4))
    }

    fn reset(&mut self) {
        self.has_pending = false;
        for channel in &mut self.pending {
            channel.fill(0.0);
        }
    }
}

struct OneChunkSourceDecoder {
    chunk: Option<PcmChunk>,
    spec: PcmSpec,
}

impl Decoder for OneChunkSourceDecoder {
    fn duration(&self) -> Option<kithara_platform::time::Duration> {
        None
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        Ok(self
            .chunk
            .take()
            .map_or(DecoderChunkOutcome::Eof, DecoderChunkOutcome::Chunk))
    }

    fn seek(&mut self, pos: kithara_platform::time::Duration) -> DecodeResult<DecoderSeekOutcome> {
        Ok(DecoderSeekOutcome::Landed {
            landed_at: pos,
            landed_frame: 0,
            landed_byte: None,
            preroll: kithara_stream::PrerollHint::NotNeeded,
        })
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn update_byte_len(&self, _len: u64) {}
}

#[test]
fn standalone_decoder_adapter_wraps_configured_backend() {
    let source_rate = NonZeroU32::new(44_100).expect("test rate");
    let target_rate = NonZeroU32::new(48_000).expect("test rate");
    let spec = PcmSpec::new(2, source_rate);
    let meta = PcmMeta {
        spec,
        frames: 4,
        ..PcmMeta::default()
    };
    let samples = PcmPool::default().attach(vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
    let decoder = Box::new(OneChunkSourceDecoder {
        chunk: Some(PcmChunk::new(meta, samples)),
        spec,
    });
    let config = DecoderResamplerConfig::builder()
        .target_sample_rate(target_rate)
        .backend(AdapterProbeBackend)
        .build();

    let pool = PcmPool::default();
    let mut decoder = wrap(decoder, Some(config), &pool).expect("standalone adapter builds");
    let output: PcmChunk = decoder
        .next_chunk()
        .expect("next chunk")
        .try_into()
        .expect("adapter output chunk");

    assert_eq!(decoder.spec(), PcmSpec::new(2, target_rate));
    assert_eq!(output.spec(), PcmSpec::new(2, target_rate));
    assert_eq!(&*output.samples, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
}

#[test]
fn standalone_decoder_adapter_flushes_backend_delay_at_eof() {
    let source_rate = NonZeroU32::new(44_100).expect("test rate");
    let target_rate = NonZeroU32::new(48_000).expect("test rate");
    let spec = PcmSpec::new(2, source_rate);
    let meta = PcmMeta {
        spec,
        frames: 4,
        ..PcmMeta::default()
    };
    let samples = PcmPool::default().attach(vec![1.0, 10.0, 2.0, 20.0, 3.0, 30.0, 4.0, 40.0]);
    let decoder = Box::new(OneChunkSourceDecoder {
        chunk: Some(PcmChunk::new(meta, samples)),
        spec,
    });
    let config = DecoderResamplerConfig::builder()
        .target_sample_rate(target_rate)
        .backend(DelayedProbeBackend)
        .build();

    let pool = PcmPool::default();
    let mut decoder = wrap(decoder, Some(config), &pool).expect("standalone adapter builds");
    let output: PcmChunk = decoder
        .next_chunk()
        .expect("next chunk")
        .try_into()
        .expect("adapter output chunk");

    assert_eq!(output.frames(), 4);
    assert_eq!(
        &*output.samples,
        &[1.0, 10.0, 2.0, 20.0, 3.0, 30.0, 4.0, 40.0]
    );
    assert!(matches!(
        decoder.next_chunk().expect("eof"),
        DecoderChunkOutcome::Eof
    ));
}
