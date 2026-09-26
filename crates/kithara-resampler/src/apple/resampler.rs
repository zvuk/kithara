use std::num::{NonZeroU32, NonZeroUsize};

use kithara_apple::audio_toolbox::{
    AUDIO_CONVERTER_ERR_NO_DATA_NOW, AUDIO_FORMAT_LINEAR_PCM, AudioConverter,
    AudioStreamBasicDescription, AudioToolboxError, BITS_PER_F32_SAMPLE, BYTES_PER_F32_SAMPLE,
    FLOAT32_PLANAR_FLAGS, NO_ERR, OSStatus, OwnedAudioBufferList, os_status_to_string,
};
use kithara_bufpool::{HasPool, PoolRegion};
use num_traits::cast::ToPrimitive;
use tracing::warn;

use super::input::AppleResamplerInputState;
use crate::{
    Resampler, ResamplerBuildError, ResamplerCapabilities, ResamplerError, ResamplerMode,
    ResamplerProcess,
};

const BACKEND_APPLE: &str = "apple-audio-converter";

pub struct AppleResampler {
    converter: AudioConverter,
    input_state: Box<AppleResamplerInputState>,
    channels: NonZeroUsize,
    output_list: OwnedAudioBufferList,
    mode: ResamplerMode,
    chunk_size: usize,
}

impl AppleResampler {
    /// Build a standalone `CoreAudio` PCM-to-PCM sample-rate converter.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerBuildError`] if the fixed shape is invalid or
    /// `audio_converter_new` rejects the planar PCM ASBD pair.
    pub(crate) fn new<S>(
        source_rate: u32,
        target_rate: u32,
        channels: usize,
        chunk_size: usize,
        pools: &PoolRegion<S>,
    ) -> Result<Self, ResamplerBuildError>
    where
        S: HasPool<f32>,
    {
        let channels = NonZeroUsize::new(channels)
            .ok_or_else(|| apple_build_config("channel count must be non-zero"))?;
        if chunk_size == 0 {
            return Err(apple_build_config("chunk size must be non-zero"));
        }
        let source_sample_rate =
            NonZeroU32::new(source_rate).ok_or(ResamplerBuildError::InvalidSampleRate {
                resource: "source",
                rate: source_rate,
            })?;
        let target_sample_rate =
            NonZeroU32::new(target_rate).ok_or(ResamplerBuildError::InvalidSampleRate {
                resource: "target",
                rate: target_rate,
            })?;

        let source_format = planar_f32_asbd(source_sample_rate, channels)?;
        let target_format = planar_f32_asbd(target_sample_rate, channels)?;
        let converter = AudioConverter::new(&source_format, &target_format)
            .map_err(|err| apple_build_status("AudioConverterNew", err_status(&err)))?;

        let input_state = Box::new(
            AppleResamplerInputState::new(channels.get(), chunk_size, pools).map_err(|err| {
                ResamplerBuildError::BackendBuild {
                    backend: BACKEND_APPLE,
                    detail: err.to_string(),
                }
            })?,
        );
        let output_list = OwnedAudioBufferList::new(channels.get())
            .map_err(|err| apple_build_config_owned(err.to_string()))?;
        Ok(Self {
            converter,
            input_state,
            output_list,
            channels,
            chunk_size,
            mode: ResamplerMode::FixedRatio {
                source_sample_rate,
                target_sample_rate,
            },
        })
    }

    fn fill_output(
        &mut self,
        output: &mut [&mut [f32]],
    ) -> Result<ResamplerProcess, ResamplerError> {
        let output_frames = validate_output(output, self.channels.get())?;
        if output_frames == 0 {
            return Ok(ResamplerProcess::new(0, 0));
        }

        self.output_list
            .set_planar_f32_output(output, output_frames)
            .map_err(|err| apple_buffer_owned(err.to_string()))?;
        let mut output_packets = frames_to_u32(output_frames)?;

        let status = self.converter.fill_complex_buffer(
            self.input_state.as_mut(),
            self.channels.get(),
            &mut output_packets,
            &mut self.output_list,
        );

        if status != NO_ERR && status != AUDIO_CONVERTER_ERR_NO_DATA_NOW {
            return Err(apple_error_status(
                "AudioConverterFillComplexBuffer",
                status,
            ));
        }

        let produced = usize::try_from(output_packets)
            .map_err(|_| apple_buffer("output frame count exceeds usize"))?;
        Ok(ResamplerProcess::new(self.input_state.consumed(), produced))
    }

    fn ratio(&self) -> f64 {
        let ResamplerMode::FixedRatio {
            source_sample_rate,
            target_sample_rate,
        } = self.mode
        else {
            return 1.0;
        };
        f64::from(target_sample_rate.get()) / f64::from(source_sample_rate.get())
    }
}

impl Resampler for AppleResampler {
    fn capabilities(&self) -> ResamplerCapabilities {
        ResamplerCapabilities::FIXED_RATIO
            | ResamplerCapabilities::REPORTS_LATENCY
            | ResamplerCapabilities::STANDALONE
    }

    fn channels(&self) -> NonZeroUsize {
        self.channels
    }

    fn drain_into_buffer(&mut self, output: &mut [&mut [f32]]) -> Result<usize, ResamplerError> {
        self.input_state.stage_empty_eos();
        let process = self.fill_output(output)?;
        Ok(process.output_frames)
    }

    fn flush_into_buffer(
        &mut self,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
    ) -> Result<ResamplerProcess, ResamplerError> {
        self.input_state.stage(input, self.chunk_size, true)?;
        self.fill_output(output)
    }

    /// Caller-facing adapter quantum; `CoreAudio` pulls input through the callback.
    fn input_frames_max(&self) -> usize {
        self.chunk_size
    }

    /// Caller-facing adapter quantum; `CoreAudio` pulls input through the callback.
    fn input_frames_next(&self) -> usize {
        self.chunk_size
    }

    fn mode(&self) -> ResamplerMode {
        self.mode
    }

    fn output_frames_for_input(&self, input_frames: usize) -> usize {
        let Some(input_frames) = input_frames.to_f64() else {
            return usize::MAX;
        };
        let frames = (input_frames * self.ratio()).ceil();
        if !frames.is_finite() || frames <= 0.0 {
            return 0;
        }

        frames.to_usize().unwrap_or(usize::MAX)
    }

    fn output_frames_max(&self) -> usize {
        self.output_frames_for_input(self.chunk_size)
    }

    fn output_frames_next(&self) -> usize {
        self.output_frames_for_input(self.chunk_size)
    }

    fn process_into_buffer(
        &mut self,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
    ) -> Result<ResamplerProcess, ResamplerError> {
        self.input_state.stage(input, self.chunk_size, false)?;
        self.fill_output(output)
    }

    fn reset(&mut self) {
        let status = self.converter.reset();
        if status != NO_ERR {
            warn!(
                status,
                "AppleResampler: audio_converter_reset returned non-zero"
            );
        }
        self.input_state.clear();
    }
}

fn planar_f32_asbd(
    sample_rate: NonZeroU32,
    channels: NonZeroUsize,
) -> Result<AudioStreamBasicDescription, ResamplerBuildError> {
    let channels = u32::try_from(channels.get())
        .map_err(|_| apple_build_config("channel count exceeds CoreAudio limit"))?;
    Ok(AudioStreamBasicDescription {
        sample_rate: f64::from(sample_rate.get()),
        format_id: AUDIO_FORMAT_LINEAR_PCM,
        format_flags: FLOAT32_PLANAR_FLAGS,
        bytes_per_packet: BYTES_PER_F32_SAMPLE,
        frames_per_packet: 1,
        bytes_per_frame: BYTES_PER_F32_SAMPLE,
        channels_per_frame: channels,
        bits_per_channel: BITS_PER_F32_SAMPLE,
        ..Default::default()
    })
}

fn validate_output(output: &[&mut [f32]], channels: usize) -> Result<usize, ResamplerError> {
    if output.len() != channels {
        return Err(apple_buffer("output channel count mismatch"));
    }
    let frames = output
        .first()
        .map(|channel| channel.len())
        .ok_or_else(|| apple_buffer("missing output channel"))?;
    if output.iter().any(|channel| channel.len() != frames) {
        return Err(apple_buffer("output channel lengths differ"));
    }
    Ok(frames)
}

fn frames_to_u32(frames: usize) -> Result<u32, ResamplerError> {
    u32::try_from(frames).map_err(|_| apple_buffer("frame count exceeds CoreAudio limit"))
}

pub(super) fn apple_build_config(detail: &'static str) -> ResamplerBuildError {
    ResamplerBuildError::BackendBuild {
        detail: detail.into(),
        backend: BACKEND_APPLE,
    }
}

const fn apple_build_config_owned(detail: String) -> ResamplerBuildError {
    ResamplerBuildError::BackendBuild {
        detail,
        backend: BACKEND_APPLE,
    }
}

fn apple_build_status(op: &'static str, status: OSStatus) -> ResamplerBuildError {
    ResamplerBuildError::BackendBuild {
        detail: format!("{op}: {}", os_status_to_string(status)),
        backend: BACKEND_APPLE,
    }
}

const fn apple_buffer(detail: &'static str) -> ResamplerError {
    ResamplerError::InvalidBuffer { detail }
}

const fn apple_buffer_owned(detail: String) -> ResamplerError {
    ResamplerError::Backend {
        detail,
        op: "apple audio buffer",
    }
}

fn apple_error_status(op: &'static str, status: OSStatus) -> ResamplerError {
    ResamplerError::Backend {
        op,
        detail: os_status_to_string(status),
    }
}

const fn err_status(err: &AudioToolboxError) -> OSStatus {
    match err {
        AudioToolboxError::Status { status, .. } => *status,
        AudioToolboxError::Config { .. } => -50,
    }
}

#[cfg(all(test, feature = "resample-rubato"))]
mod tests {

    use kithara_test_fixtures::unit_fixtures::{
        apple_planar_44100, apple_planar_48000, trim_silence,
    };
    use kithara_test_utils::kithara;
    use num_traits::cast::ToPrimitive;

    use crate::{
        Resampler, ResamplerConfig, ResamplerMode, ResamplerOptions, ResamplerQuality,
        ResamplerSettings,
        apple::AppleAudioConverterBackend,
        create_resampler,
        rubato::{RubatoBackend, RubatoResampler},
        test_pools::{TestPools, pools},
    };

    mod test_consts {
        pub(super) const CHUNK_FRAMES: usize = 1024;
        pub(super) const DRAIN_LIMIT: usize = 16;
        pub(super) const FLUSH_FRAME_TOLERANCE: usize = 1;
        pub(super) const PASSTHROUGH_RMS_TOLERANCE: f64 = 0.000_01;
        pub(super) const SHAPE_ENERGY_DELTA_TOLERANCE: f64 = 0.12;
    }

    struct Rendered {
        output: Vec<Vec<f32>>,
        contract_frames: usize,
        drain_calls: usize,
        frames: usize,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum TestBackend {
        Apple,
        Rubato,
    }

    #[kithara::test(native, flash(false))]
    fn resampler_apple_rubato_length_contract_44100_to_48000_stereo(
        apple_planar_44100: Vec<f32>,
        trim_silence: Vec<f32>,
    ) {
        assert_length_contract(44_100, 48_000, &apple_planar_44100, &trim_silence);
    }

    #[kithara::test(native, flash(false))]
    fn resampler_apple_rubato_length_contract_48000_to_44100_mono(
        apple_planar_48000: Vec<f32>,
        trim_silence: Vec<f32>,
    ) {
        assert_length_contract(48_000, 44_100, &apple_planar_48000[..1024], &trim_silence);
    }

    #[kithara::test(native, flash(false))]
    fn resampler_apple_tail_flush_drain_eventually_empty(
        apple_planar_44100: Vec<f32>,
        trim_silence: Vec<f32>,
    ) {
        let input = apple_planar_44100
            .chunks_exact(1024)
            .map(<[f32]>::to_vec)
            .collect::<Vec<_>>();
        let rendered = render(TestBackend::Apple, &input, 44_100, 48_000, &trim_silence);
        let input_energy = energy(&input);
        let output_energy = energy(&rendered.output);

        assert!(rendered.drain_calls <= test_consts::DRAIN_LIMIT);
        assert!(output_energy > input_energy * 0.5);
    }

    #[kithara::test(native, flash(false))]
    fn resampler_apple_passthrough_near_identity(
        apple_planar_48000: Vec<f32>,
        trim_silence: Vec<f32>,
    ) {
        let input = apple_planar_48000
            .chunks_exact(1024)
            .map(<[f32]>::to_vec)
            .collect::<Vec<_>>();
        let rendered = render(TestBackend::Apple, &input, 48_000, 48_000, &trim_silence);
        let rms = rms_diff(&input, &rendered.output);

        assert_len_close(
            "apple passthrough",
            rendered.frames,
            test_consts::CHUNK_FRAMES,
        );
        assert!(rms <= test_consts::PASSTHROUGH_RMS_TOLERANCE);
    }

    #[kithara::test(native, flash(false))]
    fn resampler_apple_rubato_shape_energy_close(
        apple_planar_44100: Vec<f32>,
        trim_silence: Vec<f32>,
    ) {
        let input = apple_planar_44100
            .chunks_exact(1024)
            .map(<[f32]>::to_vec)
            .collect::<Vec<_>>();
        let apple = render(TestBackend::Apple, &input, 44_100, 48_000, &trim_silence);
        let rubato = render(TestBackend::Rubato, &input, 44_100, 48_000, &trim_silence);
        let apple_energy = energy(&apple.output);
        let rubato_energy = energy(&rubato.output);
        let energy_delta = normalized_delta(apple_energy, rubato_energy);

        assert_len_close("apple/rubato shape", apple.frames, rubato.frames);
        assert!(energy_delta <= test_consts::SHAPE_ENERGY_DELTA_TOLERANCE);
    }

    fn assert_length_contract(
        source_rate: u32,
        target_rate: u32,
        pcm: &[f32],
        trim_silence: &[f32],
    ) {
        let input = pcm
            .chunks_exact(1024)
            .map(<[f32]>::to_vec)
            .collect::<Vec<_>>();
        let expected = expected_frames(test_consts::CHUNK_FRAMES, source_rate, target_rate);
        let apple = render(
            TestBackend::Apple,
            &input,
            source_rate,
            target_rate,
            trim_silence,
        );
        let rubato = render(
            TestBackend::Rubato,
            &input,
            source_rate,
            target_rate,
            trim_silence,
        );

        assert_eq!(apple.contract_frames, expected);
        assert_eq!(rubato.contract_frames, expected);
        assert_len_close("apple length", apple.frames, expected);
        assert_len_close("rubato length", rubato.frames, expected);
        assert_len_close("apple/rubato length", apple.frames, rubato.frames);
    }

    fn render(
        backend: TestBackend,
        input: &[Vec<f32>],
        source_rate: u32,
        target_rate: u32,
        silence: &[f32],
    ) -> Rendered {
        let channels = input.len();
        let frames = frame_count(input);
        match backend {
            TestBackend::Apple => {
                let mut resampler =
                    create_apple_resampler(source_rate, target_rate, channels, frames);
                render_resampler(backend, &mut resampler, input, false, silence)
            }
            TestBackend::Rubato => {
                let mut resampler =
                    create_rubato_resampler(source_rate, target_rate, channels, frames);
                render_resampler(backend, &mut resampler, input, true, silence)
            }
        }
    }

    fn render_resampler<R>(
        backend: TestBackend,
        resampler: &mut R,
        input: &[Vec<f32>],
        pump_rubato_tail: bool,
        silence: &[f32],
    ) -> Rendered
    where
        R: Resampler,
    {
        let channels = input.len();
        let frames = frame_count(input);
        let contract_frames = resampler.output_frames_for_input(frames);
        let mut output = vec![vec![0.0; contract_frames.max(1)]; channels];
        let input_refs = planar_refs(input);
        let mut output_refs = planar_refs_mut(&mut output);
        let process = resampler
            .flush_into_buffer(&input_refs, &mut output_refs)
            .unwrap_or_else(|err| panic!("flush_into_buffer({backend:?}) failed: {err}"));
        assert_eq!(process.input_frames, frames);
        truncate_planar(&mut output, process.output_frames);

        let mut drain_calls = 0;
        loop {
            let mut drain = vec![vec![0.0; contract_frames.max(1)]; channels];
            let mut drain_refs = planar_refs_mut(&mut drain);
            let produced = resampler
                .drain_into_buffer(&mut drain_refs)
                .unwrap_or_else(|err| panic!("drain_into_buffer({backend:?}) failed: {err}"));
            drain_calls += 1;
            if produced == 0 {
                break;
            }
            assert!(drain_calls <= test_consts::DRAIN_LIMIT);
            append_planar(&mut output, &drain, produced);
        }

        if pump_rubato_tail {
            zero_pump_rubato_tail(resampler, &mut output, contract_frames, silence);
        }

        let frames = frame_count(&output);
        Rendered {
            output,
            contract_frames,
            drain_calls,
            frames,
        }
    }

    fn create_apple_resampler(
        source_rate: u32,
        target_rate: u32,
        channels: usize,
        frames: usize,
    ) -> super::AppleResampler {
        let settings = test_settings(source_rate, target_rate, channels, frames);
        let config = ResamplerConfig::builder()
            .backend(AppleAudioConverterBackend::new())
            .settings(settings)
            .build();
        create_resampler(&config)
            .unwrap_or_else(|err| panic!("create_resampler(Apple) failed: {err}"))
    }

    fn create_rubato_resampler(
        source_rate: u32,
        target_rate: u32,
        channels: usize,
        frames: usize,
    ) -> RubatoResampler {
        let settings = test_settings(source_rate, target_rate, channels, frames);
        let config = ResamplerConfig::builder()
            .backend(RubatoBackend::new())
            .settings(settings)
            .build();
        create_resampler(&config)
            .unwrap_or_else(|err| panic!("create_resampler(Rubato) failed: {err}"))
    }

    fn test_settings(
        source_rate: u32,
        target_rate: u32,
        channels: usize,
        frames: usize,
    ) -> ResamplerSettings<TestPools> {
        ResamplerSettings::builder()
            .channels(std::num::NonZeroUsize::new(channels).expect("test channels"))
            .mode(ResamplerMode::FixedRatio {
                source_sample_rate: std::num::NonZeroU32::new(source_rate)
                    .expect("test source rate"),
                target_sample_rate: std::num::NonZeroU32::new(target_rate)
                    .expect("test target rate"),
            })
            .quality(ResamplerQuality::High)
            .options(ResamplerOptions::builder().chunk_size(frames).build())
            .pools(pools())
            .build()
    }

    fn zero_pump_rubato_tail<R>(
        resampler: &mut R,
        output: &mut [Vec<f32>],
        contract_frames: usize,
        silence: &[f32],
    ) where
        R: Resampler,
    {
        let channels = output.len();
        let input_frames = resampler.input_frames_next();
        let zero_input = silence[..input_frames * channels]
            .chunks_exact(input_frames)
            .map(<[f32]>::to_vec)
            .collect::<Vec<_>>();
        let zero_input_refs = planar_refs(&zero_input);
        let mut pump_count = 0;
        while frame_count(output) < contract_frames {
            let out_frames = resampler.output_frames_next().max(1);
            let mut zero_output = vec![vec![0.0; out_frames]; channels];
            let mut zero_output_refs = planar_refs_mut(&mut zero_output);
            let process = resampler
                .process_into_buffer(&zero_input_refs, &mut zero_output_refs)
                .unwrap_or_else(|err| panic!("rubato zero-pump failed: {err}"));
            pump_count += 1;
            assert!(pump_count <= test_consts::DRAIN_LIMIT);
            let needed = contract_frames.saturating_sub(frame_count(output));
            append_planar(output, &zero_output, process.output_frames.min(needed));
        }
    }

    fn planar_refs(planar: &[Vec<f32>]) -> Vec<&[f32]> {
        planar.iter().map(Vec::as_slice).collect()
    }

    fn planar_refs_mut(planar: &mut [Vec<f32>]) -> Vec<&mut [f32]> {
        planar.iter_mut().map(Vec::as_mut_slice).collect()
    }

    fn truncate_planar(planar: &mut [Vec<f32>], frames: usize) {
        for channel in planar {
            assert!(frames <= channel.len());
            channel.truncate(frames);
        }
    }

    fn append_planar(target: &mut [Vec<f32>], source: &[Vec<f32>], frames: usize) {
        for (target_channel, source_channel) in target.iter_mut().zip(source.iter()) {
            assert!(frames <= source_channel.len());
            target_channel.extend_from_slice(&source_channel[..frames]);
        }
    }

    fn frame_count(planar: &[Vec<f32>]) -> usize {
        planar.first().map_or(0, Vec::len)
    }

    fn expected_frames(input_frames: usize, source_rate: u32, target_rate: u32) -> usize {
        let ratio = f64::from(target_rate) / f64::from(source_rate);
        let input_frames = input_frames
            .to_f64()
            .expect("test input frame count fits f64");
        (input_frames * ratio)
            .ceil()
            .to_usize()
            .expect("test output frame count fits usize")
    }

    fn assert_len_close(label: &str, actual: usize, expected: usize) {
        let delta = actual.abs_diff(expected);
        assert!(
            delta <= test_consts::FLUSH_FRAME_TOLERANCE,
            "{label}: actual={actual} expected={expected} delta={delta}"
        );
    }

    fn energy(planar: &[Vec<f32>]) -> f64 {
        planar
            .iter()
            .flat_map(|channel| channel.iter())
            .map(|sample| f64::from(*sample) * f64::from(*sample))
            .sum()
    }

    fn rms_diff(left: &[Vec<f32>], right: &[Vec<f32>]) -> f64 {
        let mut sum = 0.0;
        let mut count = 0usize;
        for (left_channel, right_channel) in left.iter().zip(right.iter()) {
            for (left_sample, right_sample) in left_channel.iter().zip(right_channel.iter()) {
                let diff = f64::from(*left_sample) - f64::from(*right_sample);
                sum = diff.mul_add(diff, sum);
                count += 1;
            }
        }
        if count == 0 {
            return 0.0;
        }
        let count = count.to_f64().expect("test sample count fits f64");
        (sum / count).sqrt()
    }

    fn normalized_delta(left: f64, right: f64) -> f64 {
        let scale = left.abs().max(right.abs());
        if scale == 0.0 {
            return 0.0;
        }
        (left - right).abs() / scale
    }
}
