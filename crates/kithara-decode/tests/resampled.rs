#![cfg(any(feature = "symphonia", all(feature = "android", target_os = "android")))]
#![forbid(unsafe_code)]

use std::{
    io::Cursor,
    num::{NonZeroU32, NonZeroUsize},
    sync::{Arc, Mutex},
};

use kithara_bufpool::{
    HasPool,
    testing::{TestPools, pools as default_pools},
};
use kithara_decode::{
    DecoderChunkOutcome, DecoderConfig, DecoderFactory, DecoderResamplerConfig, DecoderSeekOutcome,
};
use kithara_platform::time::Duration;
use kithara_resampler::{
    Resampler, ResamplerBackend, ResamplerBuildError, ResamplerCapabilities, ResamplerMode,
    ResamplerProcess, ResamplerSettings,
};
use kithara_signal::{AudioChunk, AudioSpec};
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_fixtures::unit_fixtures::{
    poisoned_float_wav, resampled_markers, resampled_wav_eight, resampled_wav_four,
    resampled_wav_seek, trim_silence,
};
use kithara_test_utils::kithara;

const CHANNELS: u16 = 2;
const FRAMES: usize = 4;
const SOURCE_RATE: u32 = 44_100;
const TARGET_RATE: u32 = 48_000;
const POISON: [[f32; FRAMES]; 2] = [
    [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e-40],
    [0.25, -0.25, 0.5, -0.5],
];

fn test_spec(sample_rate: u32) -> AudioSpec {
    AudioSpec::new(
        CHANNELS,
        NonZeroU32::new(sample_rate).expect("test sample rate is non-zero"),
    )
}

fn test_duration(sample_rate: u32, frames: u64) -> Duration {
    test_spec(sample_rate)
        .duration_for(frames)
        .expect("test duration is representable")
}

fn test_frames(sample_rate: u32, duration: Duration) -> usize {
    test_spec(sample_rate)
        .frames_for(duration)
        .expect("test frame count is representable")
        .get()
}

#[derive(Clone)]
struct AdapterProbeBackend(Vec<f32>);

impl ResamplerBackend for AdapterProbeBackend {
    type Resampler = AdapterProbeResampler;

    fn build<S>(
        &self,
        settings: &ResamplerSettings<S>,
    ) -> Result<Self::Resampler, ResamplerBuildError>
    where
        S: HasPool<f32>,
    {
        Ok(AdapterProbeResampler {
            markers: self.0.clone(),
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
    markers: Vec<f32>,
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
            dst[..FRAMES].copy_from_slice(&self.markers[channel * FRAMES..(channel + 1) * FRAMES]);
        }
        Ok(ResamplerProcess::new(FRAMES, FRAMES))
    }

    fn reset(&mut self) {}
}

type Captured = Arc<Mutex<Vec<f32>>>;

#[derive(Clone)]
struct CaptureProbeBackend(Captured, Vec<f32>);

impl ResamplerBackend for CaptureProbeBackend {
    type Resampler = CaptureProbeResampler;

    fn build<S>(
        &self,
        settings: &ResamplerSettings<S>,
    ) -> Result<Self::Resampler, ResamplerBuildError>
    where
        S: HasPool<f32>,
    {
        Ok(CaptureProbeResampler {
            markers: self.1.clone(),
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
    markers: Vec<f32>,
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
            dst[..FRAMES].copy_from_slice(&self.markers[channel * FRAMES..(channel + 1) * FRAMES]);
        }
        Ok(ResamplerProcess::new(FRAMES, FRAMES))
    }

    fn reset(&mut self) {}
}

#[derive(Clone)]
struct DelayedProbeBackend(Vec<f32>, Vec<f32>);

impl ResamplerBackend for DelayedProbeBackend {
    type Resampler = DelayedProbeResampler;

    fn build<S>(
        &self,
        settings: &ResamplerSettings<S>,
    ) -> Result<Self::Resampler, ResamplerBuildError>
    where
        S: HasPool<f32>,
    {
        Ok(DelayedProbeResampler {
            silence: self.1.clone(),
            markers: self.0.clone(),
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
    silence: Vec<f32>,
    markers: Vec<f32>,
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
                dst[..FRAMES]
                    .copy_from_slice(&self.markers[channel * FRAMES..(channel + 1) * FRAMES]);
            } else {
                dst[..FRAMES].copy_from_slice(&self.silence[..FRAMES]);
            }
        }
        self.has_pending = true;
        Ok(ResamplerProcess::new(FRAMES, FRAMES))
    }

    fn reset(&mut self) {
        self.has_pending = false;
    }
}

#[kithara::test(native, flash(false))]
fn standalone_decoder_adapter_wraps_configured_backend(
    resampled_markers: Vec<f32>,
    resampled_wav_four: &'static [u8],
) {
    let target_rate = NonZeroU32::new(TARGET_RATE).expect("test rate");
    let mut decoder = decoder_over(
        resampled_wav_four.to_vec(),
        target_rate,
        AdapterProbeBackend(resampled_markers),
    );
    let output: AudioChunk = decoder
        .next_chunk()
        .expect("next chunk")
        .try_into()
        .expect("adapter output chunk");

    assert_eq!(decoder.spec().sample_rate, target_rate);
    assert_eq!(output.spec().sample_rate, target_rate);
    assert_eq!(&*output.samples, marker_samples());
}

#[kithara::test(native, flash(false))]
fn standalone_decoder_adapter_emits_one_resampler_block_per_call(
    resampled_markers: Vec<f32>,
    resampled_wav_eight: &'static [u8],
) {
    let target_rate = NonZeroU32::new(TARGET_RATE).expect("test rate");
    let mut decoder = decoder_over(
        resampled_wav_eight.to_vec(),
        target_rate,
        AdapterProbeBackend(resampled_markers),
    );

    let first: AudioChunk = decoder
        .next_chunk()
        .expect("first chunk")
        .try_into()
        .expect("first adapter output chunk");
    let second: AudioChunk = decoder
        .next_chunk()
        .expect("second chunk")
        .try_into()
        .expect("second adapter output chunk");

    assert_eq!(first.frames(), FRAMES);
    assert_eq!(second.frames(), FRAMES);
}

#[kithara::test(native, flash(false))]
fn standalone_decoder_adapter_flushes_backend_delay_at_eof(
    trim_silence: Vec<f32>,
    resampled_markers: Vec<f32>,
    resampled_wav_four: &'static [u8],
) {
    let target_rate = NonZeroU32::new(TARGET_RATE).expect("test rate");
    let mut decoder = decoder_over(
        resampled_wav_four.to_vec(),
        target_rate,
        DelayedProbeBackend(resampled_markers, trim_silence),
    );
    let output: AudioChunk = decoder
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

#[kithara::test(native, flash(false))]
fn standalone_decoder_seek_reanchors_output_to_trimmed_target(
    resampled_markers: Vec<f32>,
    resampled_wav_seek: &'static [u8],
) {
    const TARGET: Duration = Duration::from_millis(31);

    let target_rate = NonZeroU32::new(TARGET_RATE).expect("test rate");
    let mut decoder = decoder_over(
        resampled_wav_seek.to_vec(),
        target_rate,
        AdapterProbeBackend(resampled_markers),
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
    let output: AudioChunk = decoder
        .next_chunk()
        .expect("first chunk after seek")
        .try_into()
        .expect("resampled output chunk");
    let target_frame =
        u64::try_from(test_frames(TARGET_RATE, TARGET)).expect("target frame fits u64");

    assert_eq!(output.meta.frame_offset, target_frame);
    assert_eq!(output.frames(), FRAMES);
    assert_eq!(output.meta.timestamp, TARGET);
    assert_eq!(
        output.meta.timestamp,
        test_duration(TARGET_RATE, output.meta.frame_offset)
    );
}

#[kithara::test(native, flash(false))]
fn standalone_decoder_seek_rounds_timeline_frames_half_up(
    resampled_markers: Vec<f32>,
    resampled_wav_seek: &'static [u8],
) {
    const SOURCE_TARGET_FRAME: u64 = 1_441;
    const ROUNDING_TARGET_RATE: u32 = 44_085;
    #[cfg(not(target_os = "android"))]
    const EXPECTED_LANDED_FRAME: u64 = 1_152;
    #[cfg(target_os = "android")]
    const EXPECTED_LANDED_FRAME: u64 = 1_440;
    const EXPECTED_OUTPUT_FRAME: u64 = 1_441;

    let target = test_duration(SOURCE_RATE, SOURCE_TARGET_FRAME);
    let target_rate = NonZeroU32::new(ROUNDING_TARGET_RATE).expect("test rate");
    let mut decoder = decoder_over(
        resampled_wav_seek.to_vec(),
        target_rate,
        AdapterProbeBackend(resampled_markers),
    );
    let DecoderSeekOutcome::Landed {
        landed_at,
        landed_frame,
        ..
    } = decoder.seek(target).expect("seek resampled decoder")
    else {
        panic!("seek target must be inside the test WAV");
    };
    let output: AudioChunk = decoder
        .next_chunk()
        .expect("first chunk after seek")
        .try_into()
        .expect("resampled output chunk");

    assert_eq!(
        test_frames(ROUNDING_TARGET_RATE, landed_at),
        usize::try_from(EXPECTED_LANDED_FRAME - 1).expect("landing fits usize"),
        "test landing must distinguish floor from half-up"
    );
    assert_eq!(
        test_frames(ROUNDING_TARGET_RATE, target),
        1_440,
        "test target must distinguish floor from half-up"
    );
    assert_eq!(
        (landed_frame, output.meta.frame_offset),
        (EXPECTED_LANDED_FRAME, EXPECTED_OUTPUT_FRAME)
    );
    assert_eq!(output.meta.timestamp, Duration::from_nanos(32_686_854));
}

#[kithara::test(native, flash(false))]
fn resampler_never_sees_a_sample_the_file_poisoned(
    resampled_markers: Vec<f32>,
    poisoned_float_wav: &'static [u8],
) {
    let captured: Captured = Arc::default();
    let target_rate = NonZeroU32::new(TARGET_RATE).expect("test rate");
    let mut decoder = decoder_over(
        poisoned_float_wav.to_vec(),
        target_rate,
        CaptureProbeBackend(Arc::clone(&captured), resampled_markers),
    );
    let _: AudioChunk = decoder
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

#[kithara::test(native, flash(false))]
fn decoder_factory_uses_configured_pool_region(resampled_wav_four: &'static [u8]) {
    let pools = default_pools();
    assert_eq!(pools.stats().allocated_bytes, 0);
    let config: DecoderConfig<kithara_resampler::NoResamplerBackend, TestPools> =
        DecoderConfig::builder().pools(pools.clone()).build();
    let media_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    let mut decoder = DecoderFactory::create_from_media_info(
        Cursor::new(resampled_wav_four.to_vec()),
        &media_info,
        config,
    )
    .expect("decoder builds");

    assert_eq!(pools.stats().allocated_bytes, 0);
    decoder.prepare_next_chunk();
    let prepared_bytes = pools.stats().allocated_bytes;
    assert!(prepared_bytes > 0, "initial PCM uses the injected pool");
    let chunk: AudioChunk = decoder
        .next_chunk()
        .expect("next chunk")
        .try_into()
        .expect("decoded chunk");
    assert!(!chunk.samples.is_empty());
    assert_eq!(
        pools.stats().allocated_bytes,
        prepared_bytes,
        "delivering prepared PCM must not allocate another buffer"
    );
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
        .pools(default_pools())
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
