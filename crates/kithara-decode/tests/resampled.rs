#![cfg(feature = "symphonia")]
#![forbid(unsafe_code)]

use std::{
    io::Cursor,
    num::{NonZeroU32, NonZeroUsize},
    sync::{Arc, Mutex},
};

use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::{
    DecoderChunkOutcome, DecoderConfig, DecoderFactory, DecoderResamplerConfig, DecoderSeekOutcome,
    PcmChunk, duration_for_frames, frames_for_duration,
};
use kithara_platform::time::Duration;
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
const WAV_FLOAT_FORMAT: u16 = 3;
const MARKERS: [[f32; FRAMES]; 2] = [[1.0, 2.0, 3.0, 4.0], [10.0, 20.0, 30.0, 40.0]];
const POISON: [[f32; FRAMES]; 2] = [
    [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e-40],
    [0.25, -0.25, 0.5, -0.5],
];

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

type Captured = Arc<Mutex<Vec<f32>>>;

#[derive(Clone)]
struct CaptureProbeBackend(Captured);

impl ResamplerBackend for CaptureProbeBackend {
    type Resampler = CaptureProbeResampler;

    fn build(&self, settings: &ResamplerSettings) -> Result<Self::Resampler, ResamplerBuildError> {
        Ok(CaptureProbeResampler {
            captured: Arc::clone(&self.0),
            channels: settings.channels,
            mode: settings.mode,
        })
    }

    fn capabilities(&self) -> ResamplerCapabilities {
        ResamplerCapabilities::FIXED_RATIO | ResamplerCapabilities::STANDALONE
    }

    fn name(&self) -> &'static str {
        "capture-probe"
    }
}

struct CaptureProbeResampler {
    captured: Captured,
    channels: NonZeroUsize,
    mode: ResamplerMode,
}

impl Resampler for CaptureProbeResampler {
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
        input: &[&[f32]],
        output: &mut [&mut [f32]],
    ) -> Result<ResamplerProcess, kithara_resampler::ResamplerError> {
        for channel in input {
            self.captured
                .lock()
                .expect("capture probe lock")
                .extend_from_slice(channel);
        }
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
    mode: ResamplerMode,
    has_pending: bool,
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
fn standalone_decoder_seek_reanchors_output_to_trimmed_target() {
    const TARGET: Duration = Duration::from_millis(30);
    const WAV_FRAMES: usize = 4_096;

    let target_rate = NonZeroU32::new(TARGET_RATE).expect("test rate");
    let mut decoder = decoder_over(
        test_wav_with_frames(WAV_FRAMES),
        target_rate,
        AdapterProbeBackend,
    );
    let DecoderSeekOutcome::Landed { landed_at, .. } =
        decoder.seek(TARGET).expect("seek resampled decoder")
    else {
        panic!("seek target must be inside the test WAV");
    };
    assert!(
        landed_at < TARGET,
        "test requires a coarse inner landing before the requested target"
    );
    let output: PcmChunk = decoder
        .next_chunk()
        .expect("first chunk after seek")
        .try_into()
        .expect("resampled output chunk");
    let target_frame =
        u64::try_from(frames_for_duration(TARGET_RATE, TARGET)).expect("target frame fits u64");

    assert_eq!(output.meta.frame_offset, target_frame);
    assert_eq!(output.meta.timestamp, TARGET);
    assert_eq!(
        output.meta.timestamp,
        duration_for_frames(TARGET_RATE, output.meta.frame_offset)
    );
}

#[test]
fn standalone_decoder_seek_rounds_timeline_frames_half_up() {
    const SOURCE_TARGET_FRAME: u64 = 1_441;
    const ROUNDING_TARGET_RATE: u32 = 44_085;
    const EXPECTED_LANDED_FRAME: u64 = 1_152;
    const EXPECTED_OUTPUT_FRAME: u64 = 1_441;
    const WAV_FRAMES: usize = 4_096;

    let target = duration_for_frames(SOURCE_RATE, SOURCE_TARGET_FRAME);
    let target_rate = NonZeroU32::new(ROUNDING_TARGET_RATE).expect("test rate");
    let mut decoder = decoder_over(
        test_wav_with_frames(WAV_FRAMES),
        target_rate,
        AdapterProbeBackend,
    );
    let DecoderSeekOutcome::Landed {
        landed_at,
        landed_frame,
        ..
    } = decoder.seek(target).expect("seek resampled decoder")
    else {
        panic!("seek target must be inside the test WAV");
    };
    let output: PcmChunk = decoder
        .next_chunk()
        .expect("first chunk after seek")
        .try_into()
        .expect("resampled output chunk");

    assert_eq!(
        frames_for_duration(ROUNDING_TARGET_RATE, landed_at),
        1_151,
        "test landing must distinguish floor from half-up"
    );
    assert_eq!(
        frames_for_duration(ROUNDING_TARGET_RATE, target),
        1_440,
        "test target must distinguish floor from half-up"
    );
    assert_eq!(
        (landed_frame, output.meta.frame_offset),
        (EXPECTED_LANDED_FRAME, EXPECTED_OUTPUT_FRAME)
    );
    assert_eq!(output.meta.timestamp, Duration::from_nanos(32_686_854));
}

#[test]
fn resampler_never_sees_a_sample_the_file_poisoned() {
    let captured: Captured = Arc::default();
    let target_rate = NonZeroU32::new(TARGET_RATE).expect("test rate");
    let mut decoder = decoder_over(
        poisoned_float_wav(),
        target_rate,
        CaptureProbeBackend(Arc::clone(&captured)),
    );
    let _: PcmChunk = decoder
        .next_chunk()
        .expect("next chunk")
        .try_into()
        .expect("adapter output chunk");

    let seen = captured.lock().expect("capture probe lock");
    assert_eq!(
        seen.len(),
        FRAMES * usize::from(CHANNELS),
        "the probe never saw a full block"
    );
    let leaked: Vec<f32> = seen
        .iter()
        .copied()
        .filter(|sample| {
            !sample.is_finite() || (*sample != 0.0 && sample.abs() < f32::MIN_POSITIVE)
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "the file's poison reached the resampler: {leaked:?}"
    );
    assert_eq!(
        seen[FRAMES..],
        POISON[1],
        "the untouched channel lost its samples"
    );
    drop(seen);
}

#[test]
fn decoder_factory_uses_configured_pcm_pool() {
    let pcm_pool = PcmPool::new(4, 4_096);
    let config: DecoderConfig = DecoderConfig::builder()
        .byte_pool(BytePool::new(4, 4_096))
        .pcm_pool(pcm_pool.clone())
        .build();
    let media_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
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
    decoder_over(test_wav(), target_rate, backend)
}

fn decoder_over<B>(
    wav: Vec<u8>,
    target_rate: NonZeroU32,
    backend: B,
) -> Box<dyn kithara_decode::Decoder>
where
    B: ResamplerBackend,
{
    let media_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
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
    DecoderFactory::create_from_media_info(Cursor::new(wav), &media_info, config)
        .expect("decoder builds")
}

const fn marker_samples() -> &'static [f32] {
    &[1.0, 10.0, 2.0, 20.0, 3.0, 30.0, 4.0, 40.0]
}

fn test_wav() -> Vec<u8> {
    test_wav_with_frames(FRAMES)
}

fn test_wav_with_frames(frames: usize) -> Vec<u8> {
    let data_size = frames
        .saturating_mul(usize::from(CHANNELS))
        .saturating_mul(usize::from(WAV_BYTES_PER_SAMPLE));
    let mut wav = wav_header(WAV_PCM_FORMAT, WAV_BITS_PER_SAMPLE, data_size);
    wav.resize(WAV_HEADER_SIZE + data_size, 0);
    wav
}

fn poisoned_float_wav() -> Vec<u8> {
    const BYTES_PER_SAMPLE: u16 = 4;
    const BITS_PER_SAMPLE: u16 = BYTES_PER_SAMPLE * 8;

    let data_size = FRAMES
        .saturating_mul(usize::from(CHANNELS))
        .saturating_mul(usize::from(BYTES_PER_SAMPLE));
    let mut wav = wav_header(WAV_FLOAT_FORMAT, BITS_PER_SAMPLE, data_size);
    for frame in 0..FRAMES {
        for channel in POISON {
            wav.extend_from_slice(&channel[frame].to_le_bytes());
        }
    }
    wav
}

fn wav_header(format: u16, bits_per_sample: u16, data_size: usize) -> Vec<u8> {
    let bytes_per_sample = bits_per_sample / 8;
    let data_size_u32 = u32::try_from(data_size).expect("test WAV data size fits u32");
    let mut wav = Vec::with_capacity(WAV_HEADER_SIZE + data_size);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(WAV_DATA_OFFSET + data_size_u32).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&WAV_FMT_CHUNK_SIZE.to_le_bytes());
    wav.extend_from_slice(&format.to_le_bytes());
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&SOURCE_RATE.to_le_bytes());
    wav.extend_from_slice(
        &(SOURCE_RATE * u32::from(CHANNELS) * u32::from(bytes_per_sample)).to_le_bytes(),
    );
    wav.extend_from_slice(&(CHANNELS * bytes_per_sample).to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size_u32.to_le_bytes());
    wav
}
