#![allow(unsafe_code)]

use std::{ffi::c_void, mem::size_of, ptr};

use num_traits::cast::ToPrimitive;
use tracing::warn;

use super::{
    consts::Consts,
    ffi::{
        AudioConverterDispose, AudioConverterFillComplexBuffer, AudioConverterNew,
        AudioConverterRef, AudioConverterReset, AudioStreamBasicDescription, UInt32,
    },
};
use crate::resampler::{Resampler, ResamplerBuildError, ResamplerError};

mod buffer;
mod input;

use buffer::PlanarAudioBufferList;
use input::{AppleResamplerInputState, apple_resampler_input_callback};

mod constants {
    use std::mem::size_of;

    use crate::apple::{
        consts::Consts,
        ffi::{AudioBuffer, AudioBufferList, AudioFormatFlags},
    };

    pub(super) const AUDIO_BUFFER_LIST_HEADER_BYTES: usize =
        size_of::<AudioBufferList>() - size_of::<AudioBuffer>();
    pub(super) const AUDIO_FORMAT_FLAG_IS_NON_INTERLEAVED: AudioFormatFlags = 1 << 5;
    pub(super) const AUDIO_FORMAT_FLAGS_NATIVE_FLOAT_PLANAR: AudioFormatFlags =
        Consts::kAudioFormatFlagsNativeFloatPacked | AUDIO_FORMAT_FLAG_IS_NON_INTERLEAVED;
}

pub(crate) struct AppleResampler {
    converter: AudioConverterRef,
    input_state: Box<AppleResamplerInputState>,
    output_list: PlanarAudioBufferList,
    source_rate: u32,
    target_rate: u32,
    chunk_size: usize,
}

// SAFETY: `AudioConverterRef` is an opaque CoreAudio handle used through
// mutable `AppleResampler` methods only; the resampler is never shared.
unsafe impl Send for AppleResampler {}

impl AppleResampler {
    /// Build a standalone `CoreAudio` PCM-to-PCM sample-rate converter.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerBuildError`] if the fixed shape is invalid or
    /// `AudioConverterNew` rejects the planar PCM ASBD pair.
    pub(crate) fn new(
        source_rate: u32,
        target_rate: u32,
        channels: usize,
        chunk_size: usize,
    ) -> Result<Self, ResamplerBuildError> {
        if channels == 0 {
            return Err(ResamplerBuildError::apple_config(
                "channel count must be non-zero",
            ));
        }
        if chunk_size == 0 {
            return Err(ResamplerBuildError::apple_config(
                "chunk size must be non-zero",
            ));
        }

        let source_format = planar_f32_asbd(source_rate, channels)?;
        let target_format = planar_f32_asbd(target_rate, channels)?;
        let mut converter = ptr::null_mut();
        // SAFETY: source/target ASBDs are valid stack values; `converter` is a writable out-param.
        let status = unsafe { AudioConverterNew(&source_format, &target_format, &mut converter) };
        if status != Consts::noErr {
            return Err(ResamplerBuildError::apple_status(
                "AudioConverterNew",
                status,
            ));
        }

        let input_state = Box::new(AppleResamplerInputState::new(channels, chunk_size));
        let output_list = PlanarAudioBufferList::new(channels)?;
        Ok(Self {
            converter,
            input_state,
            output_list,
            source_rate,
            target_rate,
            chunk_size,
        })
    }

    fn fill_output(&mut self, output: &mut [Vec<f32>]) -> Result<(usize, usize), ResamplerError> {
        let output_frames = validate_output(output, self.input_state.channels())?;
        if output_frames == 0 {
            return Ok((0, 0));
        }

        self.output_list.set_output(output, output_frames)?;
        let mut output_packets = frames_to_u32(output_frames)?;
        let input_ptr =
            ptr::from_mut::<AppleResamplerInputState>(self.input_state.as_mut()).cast::<c_void>();

        // SAFETY: `self.converter` is live; `input_ptr` points to boxed state
        // kept alive for the full synchronous call; `output_list` points to
        // caller-owned output buffers sized above.
        let status = unsafe {
            AudioConverterFillComplexBuffer(
                self.converter,
                apple_resampler_input_callback,
                input_ptr,
                &mut output_packets,
                self.output_list.as_mut_ptr(),
                ptr::null_mut(),
            )
        };

        if status != Consts::noErr && status != Consts::kAudioConverterErr_NoDataNow {
            return Err(ResamplerError::apple_status(
                "AudioConverterFillComplexBuffer",
                status,
            ));
        }

        let produced = usize::try_from(output_packets)
            .map_err(|_| ResamplerError::apple_buffer("output frame count exceeds usize"))?;
        Ok((self.input_state.consumed(), produced))
    }
}

impl Drop for AppleResampler {
    fn drop(&mut self) {
        if !self.converter.is_null() {
            // SAFETY: `converter` was returned by `AudioConverterNew`.
            let _ = unsafe { AudioConverterDispose(self.converter) };
        }
    }
}

impl Resampler for AppleResampler {
    fn channels(&self) -> usize {
        self.input_state.channels()
    }

    fn drain_into_buffer(&mut self, output: &mut [Vec<f32>]) -> Result<usize, ResamplerError> {
        self.input_state.stage_empty_eos();
        let (_, produced) = self.fill_output(output)?;
        Ok(produced)
    }

    fn flush_into_buffer(
        &mut self,
        input: &[Vec<f32>],
        output: &mut [Vec<f32>],
    ) -> Result<(usize, usize), ResamplerError> {
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
        input: &[Vec<f32>],
        output: &mut [Vec<f32>],
    ) -> Result<(usize, usize), ResamplerError> {
        self.input_state.stage(input, self.chunk_size, false)?;
        self.fill_output(output)
    }

    fn ratio(&self) -> f64 {
        f64::from(self.target_rate) / f64::from(self.source_rate)
    }

    fn reset(&mut self) {
        // SAFETY: `converter` is a live handle owned by this resampler.
        let status = unsafe { AudioConverterReset(self.converter) };
        if status != Consts::noErr {
            warn!(
                status,
                "AppleResampler: AudioConverterReset returned non-zero"
            );
        }
        self.input_state.clear();
    }

    fn source_rate(&self) -> u32 {
        self.source_rate
    }

    fn target_rate(&self) -> u32 {
        self.target_rate
    }
}

fn planar_f32_asbd(
    sample_rate: u32,
    channels: usize,
) -> Result<AudioStreamBasicDescription, ResamplerBuildError> {
    let channels = u32::try_from(channels)
        .map_err(|_| ResamplerBuildError::apple_config("channel count exceeds CoreAudio limit"))?;
    Ok(AudioStreamBasicDescription {
        mSampleRate: f64::from(sample_rate),
        mFormatID: Consts::kAudioFormatLinearPCM,
        mFormatFlags: constants::AUDIO_FORMAT_FLAGS_NATIVE_FLOAT_PLANAR,
        mBytesPerPacket: Consts::BYTES_PER_F32_SAMPLE,
        mFramesPerPacket: 1,
        mBytesPerFrame: Consts::BYTES_PER_F32_SAMPLE,
        mChannelsPerFrame: channels,
        mBitsPerChannel: Consts::BITS_PER_F32_SAMPLE,
        ..Default::default()
    })
}

fn validate_output(output: &[Vec<f32>], channels: usize) -> Result<usize, ResamplerError> {
    if output.len() != channels {
        return Err(ResamplerError::apple_buffer(
            "output channel count mismatch",
        ));
    }
    let frames = output
        .first()
        .map(Vec::len)
        .ok_or_else(|| ResamplerError::apple_buffer("missing output channel"))?;
    if output.iter().any(|channel| channel.len() != frames) {
        return Err(ResamplerError::apple_buffer(
            "output channel lengths differ",
        ));
    }
    Ok(frames)
}

fn frames_to_u32(frames: usize) -> Result<UInt32, ResamplerError> {
    UInt32::try_from(frames)
        .map_err(|_| ResamplerError::apple_buffer("frame count exceeds CoreAudio limit"))
}

fn channel_byte_len(frames: usize) -> Result<UInt32, ResamplerError> {
    let bytes = frames
        .checked_mul(size_of::<f32>())
        .ok_or_else(|| ResamplerError::apple_buffer("channel byte size overflow"))?;
    UInt32::try_from(bytes)
        .map_err(|_| ResamplerError::apple_buffer("channel byte size exceeds CoreAudio limit"))
}

#[cfg(all(test, feature = "resample-rubato"))]
mod tests {
    use std::f32::consts::TAU;

    use kithara_test_utils::kithara;

    use crate::{
        ResamplerQuality,
        resampler::{ResamplerBackend, create_resampler},
    };

    mod test_consts {
        pub(super) const CHUNK_FRAMES: usize = 1024;
        pub(super) const DRAIN_LIMIT: usize = 16;
        pub(super) const FLUSH_FRAME_TOLERANCE: usize = 1;
        pub(super) const PASSTHROUGH_RMS_TOLERANCE: f64 = 0.000_01;
        pub(super) const SHAPE_ENERGY_DELTA_TOLERANCE: f64 = 0.12;
    }

    struct Rendered {
        contract_frames: usize,
        drain_calls: usize,
        frames: usize,
        output: Vec<Vec<f32>>,
    }

    #[kithara::test]
    fn resampler_apple_rubato_length_contract_44100_to_48000_stereo() {
        assert_length_contract(44_100, 48_000, 2);
    }

    #[kithara::test]
    fn resampler_apple_rubato_length_contract_48000_to_44100_mono() {
        assert_length_contract(48_000, 44_100, 1);
    }

    #[kithara::test]
    fn resampler_apple_tail_flush_drain_eventually_empty() {
        let input = planar_signal(2, test_consts::CHUNK_FRAMES, 44_100);
        let rendered = render(ResamplerBackend::Apple, &input, 44_100, 48_000);
        let input_energy = energy(&input);
        let output_energy = energy(&rendered.output);

        println!(
            "resampler parity tail: frames={} drain_calls={} input_energy={:.6} output_energy={:.6}",
            rendered.frames, rendered.drain_calls, input_energy, output_energy
        );
        assert!(rendered.drain_calls <= test_consts::DRAIN_LIMIT);
        assert!(output_energy > input_energy * 0.5);
    }

    #[kithara::test]
    fn resampler_apple_passthrough_near_identity() {
        let input = planar_signal(2, test_consts::CHUNK_FRAMES, 48_000);
        let rendered = render(ResamplerBackend::Apple, &input, 48_000, 48_000);
        let rms = rms_diff(&input, &rendered.output);

        println!(
            "resampler parity passthrough: frames={} rms={:.9} tolerance={:.9}",
            rendered.frames,
            rms,
            test_consts::PASSTHROUGH_RMS_TOLERANCE
        );
        assert_len_close(
            "apple passthrough",
            rendered.frames,
            test_consts::CHUNK_FRAMES,
        );
        assert!(rms <= test_consts::PASSTHROUGH_RMS_TOLERANCE);
    }

    #[kithara::test]
    fn resampler_apple_rubato_shape_energy_close() {
        let input = planar_signal(2, test_consts::CHUNK_FRAMES, 44_100);
        let apple = render(ResamplerBackend::Apple, &input, 44_100, 48_000);
        let rubato = render(ResamplerBackend::Rubato, &input, 44_100, 48_000);
        let apple_energy = energy(&apple.output);
        let rubato_energy = energy(&rubato.output);
        let energy_delta = normalized_delta(apple_energy, rubato_energy);

        println!(
            "resampler parity shape: apple_frames={} rubato_frames={} apple_energy={:.6} rubato_energy={:.6} normalized_energy_delta={:.6} tolerance={:.6}",
            apple.frames,
            rubato.frames,
            apple_energy,
            rubato_energy,
            energy_delta,
            test_consts::SHAPE_ENERGY_DELTA_TOLERANCE
        );
        assert_len_close("apple/rubato shape", apple.frames, rubato.frames);
        assert!(energy_delta <= test_consts::SHAPE_ENERGY_DELTA_TOLERANCE);
    }

    fn assert_length_contract(source_rate: u32, target_rate: u32, channels: usize) {
        let input = planar_signal(channels, test_consts::CHUNK_FRAMES, source_rate);
        let expected = expected_frames(test_consts::CHUNK_FRAMES, source_rate, target_rate);
        let apple = render(ResamplerBackend::Apple, &input, source_rate, target_rate);
        let rubato = render(ResamplerBackend::Rubato, &input, source_rate, target_rate);

        println!(
            "resampler parity length: source_rate={} target_rate={} channels={} expected={} apple_frames={} rubato_frames={} apple_contract={} rubato_contract={}",
            source_rate,
            target_rate,
            channels,
            expected,
            apple.frames,
            rubato.frames,
            apple.contract_frames,
            rubato.contract_frames
        );
        assert_eq!(apple.contract_frames, expected);
        assert_eq!(rubato.contract_frames, expected);
        assert_len_close("apple length", apple.frames, expected);
        assert_len_close("rubato length", rubato.frames, expected);
        assert_len_close("apple/rubato length", apple.frames, rubato.frames);
    }

    fn render(
        backend: ResamplerBackend,
        input: &[Vec<f32>],
        source_rate: u32,
        target_rate: u32,
    ) -> Rendered {
        let channels = input.len();
        let frames = frame_count(input);
        let mut resampler = create_resampler(
            backend,
            ResamplerQuality::High,
            source_rate,
            target_rate,
            channels,
            frames,
        )
        .unwrap_or_else(|err| panic!("create_resampler({backend:?}) failed: {err}"));
        let contract_frames = resampler.output_frames_for_input(frames);
        let mut output = vec![vec![0.0; contract_frames.max(1)]; channels];
        let (consumed, produced) = resampler
            .flush_into_buffer(input, &mut output)
            .unwrap_or_else(|err| panic!("flush_into_buffer({backend:?}) failed: {err}"));
        assert_eq!(consumed, frames);
        truncate_planar(&mut output, produced);

        let mut drain_calls = 0;
        loop {
            let mut drain = vec![vec![0.0; contract_frames.max(1)]; channels];
            let produced = resampler
                .drain_into_buffer(&mut drain)
                .unwrap_or_else(|err| panic!("drain_into_buffer({backend:?}) failed: {err}"));
            drain_calls += 1;
            if produced == 0 {
                break;
            }
            assert!(drain_calls <= test_consts::DRAIN_LIMIT);
            append_planar(&mut output, &drain, produced);
        }

        if backend == ResamplerBackend::Rubato {
            zero_pump_rubato_tail(resampler.as_mut(), &mut output, contract_frames);
        }

        let frames = frame_count(&output);
        Rendered {
            contract_frames,
            drain_calls,
            frames,
            output,
        }
    }

    fn planar_signal(channels: usize, frames: usize, sample_rate: u32) -> Vec<Vec<f32>> {
        (0..channels)
            .map(|channel| {
                let frequency = 110.0 + channel as f32 * 27.5;
                (0..frames)
                    .map(|frame| {
                        let t = frame as f32 / sample_rate as f32;
                        (TAU * frequency * t).sin() * 0.5
                    })
                    .collect()
            })
            .collect()
    }

    fn zero_pump_rubato_tail(
        resampler: &mut dyn crate::resampler::Resampler,
        output: &mut [Vec<f32>],
        contract_frames: usize,
    ) {
        let channels = output.len();
        let input_frames = resampler.input_frames_next();
        let zero_input = vec![vec![0.0; input_frames]; channels];
        let mut pump_count = 0;
        while frame_count(output) < contract_frames {
            let out_frames = resampler.output_frames_next().max(1);
            let mut zero_output = vec![vec![0.0; out_frames]; channels];
            let (_, produced) = resampler
                .process_into_buffer(&zero_input, &mut zero_output)
                .unwrap_or_else(|err| panic!("rubato zero-pump failed: {err}"));
            pump_count += 1;
            assert!(pump_count <= test_consts::DRAIN_LIMIT);
            let needed = contract_frames.saturating_sub(frame_count(output));
            append_planar(output, &zero_output, produced.min(needed));
        }
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
        (input_frames as f64 * ratio).ceil() as usize
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
                sum += diff * diff;
                count += 1;
            }
        }
        if count == 0 {
            return 0.0;
        }
        (sum / count as f64).sqrt()
    }

    fn normalized_delta(left: f64, right: f64) -> f64 {
        let scale = left.abs().max(right.abs());
        if scale == 0.0 {
            return 0.0;
        }
        (left - right).abs() / scale
    }
}
