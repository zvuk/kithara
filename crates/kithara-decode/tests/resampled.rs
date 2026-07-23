#![cfg(feature = "symphonia")]
#![forbid(unsafe_code)]

use std::{
    io::Cursor,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::{
    DecoderChunkOutcome, DecoderConfig, DecoderFactory, DecoderResamplerConfig, PcmChunk,
};
use kithara_resampler::{
    Resampler, ResamplerBackend, ResamplerBuildError, ResamplerCapabilities, ResamplerMode,
    ResamplerProcess, ResamplerSettings,
};
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};

const CHANNELS: u16 = 2;
const FRAMES: usize = 4;
const SOURCE_RATE: u32 = 44_100;
const TARGET_RATE: u32 = 48_000;
const WAV_BITS_PER_SAMPLE: u16 = 16;
const WAV_BYTES_PER_SAMPLE: u16 = WAV_BITS_PER_SAMPLE / 8;
const WAV_DATA_OFFSET: u32 = 36;
const WAV_FMT_CHUNK_SIZE: u32 = 16;
const WAV_HEADER_SIZE: usize = 44;
const WAV_PCM_FORMAT: u16 = 1;
const MARKERS: [[f32; FRAMES]; 2] = [[1.0, 2.0, 3.0, 4.0], [10.0, 20.0, 30.0, 40.0]];

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
        FRAMES
    }

    fn input_frames_next(&self) -> usize {
        FRAMES
    }

    fn mode(&self) -> ResamplerMode {
        self.mode
    }

    fn output_frames_for_input(&self, input_frames: usize) -> usize {
        input_frames
    }

    fn output_frames_max(&self) -> usize {
        FRAMES
    }

    fn output_frames_next(&self) -> usize {
        FRAMES
    }

    fn process_into_buffer(
        &mut self,
        _input: &[&[f32]],
        output: &mut [&mut [f32]],
    ) -> Result<ResamplerProcess, kithara_resampler::ResamplerError> {
        for (channel, dst) in output.iter_mut().enumerate() {
            dst[..FRAMES].copy_from_slice(&MARKERS[channel]);
        }
        Ok(ResamplerProcess::new(FRAMES, FRAMES))
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
}

impl Resampler for DelayedProbeResampler {
    fn capabilities(&self) -> ResamplerCapabilities {
        ResamplerCapabilities::FIXED_RATIO | ResamplerCapabilities::STANDALONE
    }

    fn channels(&self) -> NonZeroUsize {
        self.channels
    }

    fn input_frames_max(&self) -> usize {
        FRAMES
    }

    fn input_frames_next(&self) -> usize {
        FRAMES
    }

    fn mode(&self) -> ResamplerMode {
        self.mode
    }

    fn output_delay(&self) -> usize {
        FRAMES
    }

    fn output_frames_for_input(&self, input_frames: usize) -> usize {
        input_frames
    }

    fn output_frames_max(&self) -> usize {
        FRAMES
    }

    fn output_frames_next(&self) -> usize {
        FRAMES
    }

    fn process_into_buffer(
        &mut self,
        _input: &[&[f32]],
        output: &mut [&mut [f32]],
    ) -> Result<ResamplerProcess, kithara_resampler::ResamplerError> {
        for (channel, dst) in output.iter_mut().enumerate() {
            if self.has_pending {
                dst[..FRAMES].copy_from_slice(&MARKERS[channel]);
            } else {
                dst[..FRAMES].fill(0.0);
            }
        }
        self.has_pending = true;
        Ok(ResamplerProcess::new(FRAMES, FRAMES))
    }

    fn reset(&mut self) {
        self.has_pending = false;
    }
}

#[test]
fn standalone_decoder_adapter_wraps_configured_backend() {
    let target_rate = NonZeroU32::new(TARGET_RATE).expect("test rate");
    let mut decoder = decoder_with_resampler(target_rate, AdapterProbeBackend);
    let output: PcmChunk = decoder
        .next_chunk()
        .expect("next chunk")
        .try_into()
        .expect("adapter output chunk");

    assert_eq!(decoder.spec().sample_rate, target_rate);
    assert_eq!(output.spec().sample_rate, target_rate);
    assert_eq!(&*output.samples, marker_samples());
}

#[test]
fn standalone_decoder_adapter_flushes_backend_delay_at_eof() {
    let target_rate = NonZeroU32::new(TARGET_RATE).expect("test rate");
    let mut decoder = decoder_with_resampler(target_rate, DelayedProbeBackend);
    let output: PcmChunk = decoder
        .next_chunk()
        .expect("next chunk")
        .try_into()
        .expect("adapter output chunk");

    assert_eq!(output.frames(), FRAMES);
    assert_eq!(&*output.samples, marker_samples());
    assert!(matches!(
        decoder.next_chunk().expect("eof"),
        DecoderChunkOutcome::Eof
    ));
}

#[test]
fn decoder_factory_uses_configured_pcm_pool() {
    let pcm_pool = PcmPool::new(4, 4_096);
    let config: DecoderConfig = DecoderConfig::builder()
        .byte_pool(BytePool::new(4, 4_096))
        .pcm_pool(pcm_pool.clone())
        .build();
    let media_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let mut decoder =
        DecoderFactory::create_from_media_info(Cursor::new(test_wav()), &media_info, config)
            .expect("decoder builds");

    assert_eq!(pcm_pool.allocated_bytes(), 0);
    let chunk: PcmChunk = decoder
        .next_chunk()
        .expect("next chunk")
        .try_into()
        .expect("decoded chunk");
    assert!(!chunk.samples.is_empty());
    assert!(pcm_pool.allocated_bytes() > 0);
}

fn decoder_with_resampler<B>(
    target_rate: NonZeroU32,
    backend: B,
) -> Box<dyn kithara_decode::Decoder>
where
    B: ResamplerBackend,
{
    let media_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = DecoderConfig::builder()
        .byte_pool(BytePool::default())
        .pcm_pool(PcmPool::default())
        .resampler(
            DecoderResamplerConfig::builder()
                .target_sample_rate(target_rate)
                .backend(backend)
                .build(),
        )
        .build();
    DecoderFactory::create_from_media_info(Cursor::new(test_wav()), &media_info, config)
        .expect("decoder builds")
}

fn marker_samples() -> &'static [f32] {
    &[1.0, 10.0, 2.0, 20.0, 3.0, 30.0, 4.0, 40.0]
}

fn test_wav() -> Vec<u8> {
    let data_size = FRAMES
        .saturating_mul(usize::from(CHANNELS))
        .saturating_mul(usize::from(WAV_BYTES_PER_SAMPLE));
    let data_size_u32 = u32::try_from(data_size).expect("test WAV data size fits u32");
    let mut wav = Vec::with_capacity(WAV_HEADER_SIZE + data_size);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(WAV_DATA_OFFSET + data_size_u32).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&WAV_FMT_CHUNK_SIZE.to_le_bytes());
    wav.extend_from_slice(&WAV_PCM_FORMAT.to_le_bytes());
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&SOURCE_RATE.to_le_bytes());
    wav.extend_from_slice(
        &(SOURCE_RATE * u32::from(CHANNELS) * u32::from(WAV_BYTES_PER_SAMPLE)).to_le_bytes(),
    );
    wav.extend_from_slice(&(CHANNELS * WAV_BYTES_PER_SAMPLE).to_le_bytes());
    wav.extend_from_slice(&WAV_BITS_PER_SAMPLE.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size_u32.to_le_bytes());
    wav.resize(WAV_HEADER_SIZE + data_size, 0);
    wav
}
