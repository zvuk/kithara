use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::PcmPool;
use kithara_resampler::{
    Resampler, ResamplerBackend, ResamplerBuildError, ResamplerCapabilities, ResamplerMode,
    ResamplerPlacement, ResamplerProcess, ResamplerSettings,
};

use super::wrap;
use crate::{
    DecodeResult, Decoder, DecoderChunkOutcome, DecoderResamplerConfig, DecoderSeekOutcome,
    PcmChunk, PcmMeta, PcmSpec,
};

struct AdapterProbeBackend;

impl ResamplerBackend for AdapterProbeBackend {
    fn build(
        &self,
        settings: &ResamplerSettings,
    ) -> Result<Box<dyn Resampler>, ResamplerBuildError> {
        Ok(Box::new(AdapterProbeResampler {
            channels: settings.channels,
            mode: settings.mode,
        }))
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
        .placement(ResamplerPlacement::Standalone)
        .backend(Box::new(AdapterProbeBackend) as Box<dyn ResamplerBackend>)
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
