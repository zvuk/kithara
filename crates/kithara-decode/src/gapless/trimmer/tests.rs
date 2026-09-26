use std::num::NonZeroU32;

use kithara_bufpool::SampleBuffer;
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_test_fixtures::unit_fixtures::{
    trim_codec_priming_drops_leading_frames_and_fades_in,
    trim_codec_priming_metadata_takes_precedence_when_combined, trim_ramp, trim_seek, trim_silence,
    trim_silence_trim_above_threshold_preserves_audio,
    trim_silence_trim_below_threshold_is_trimmed,
    trim_silence_trim_does_not_introduce_click_at_boundary,
    trim_silence_trim_min_frames_boundary_at_min, trim_silence_trim_min_frames_boundary_under_min,
    trim_silence_trim_no_op_with_immediate_content,
    trim_silence_trim_preserves_quiet_intro_below_threshold_then_above,
    trim_silence_trim_scan_window_exhausted_preserves_audio,
    trim_silence_trim_trailing_disabled_by_default, trim_silence_trim_trailing_enabled,
    trim_stereo_quiet, trim_stereo_silence, trim_trailing_sine,
};
use kithara_test_utils::kithara;

use super::GaplessTrimmer;
use crate::{
    DropChunks, GaplessInfo, GaplessTailCompensation, consts,
    gapless::heuristic::SilenceTrimParams, test_pools::pools,
};

fn sample_buffer(values: &[f32]) -> SampleBuffer {
    let mut buffer = pools()
        .get_with_len::<f32>(values.len())
        .unwrap_or_else(|error| panic!("test sample buffer: {error}"));
    buffer.copy_from_slice(values);
    buffer
}

fn chunk(spec: AudioSpec, frame_offset: u64, frames: usize, input: &[f32]) -> AudioChunk {
    let samples = frames.saturating_mul(usize::from(spec.channels));
    let pcm = &input[..samples];
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            frame_offset,
            ..Default::default()
        },
        sample_buffer(&pcm),
    )
}

fn silent_chunk(spec: AudioSpec, frame_offset: u64, frames: usize, input: &[f32]) -> AudioChunk {
    let samples = frames.saturating_mul(usize::from(spec.channels));
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            frame_offset,
            ..Default::default()
        },
        sample_buffer(&input[..samples]),
    )
}

fn custom_chunk(spec: AudioSpec, frame_offset: u64, pcm: Vec<f32>) -> AudioChunk {
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            frame_offset,
            ..Default::default()
        },
        sample_buffer(&pcm),
    )
}

fn mono_spec() -> AudioSpec {
    AudioSpec::new(1, NonZeroU32::new(48_000).expect("test rate"))
}

fn stereo_spec() -> AudioSpec {
    AudioSpec::new(2, NonZeroU32::new(48_000).expect("test rate"))
}

fn fade_frames_for(spec: AudioSpec) -> usize {
    let computed = (u64::from(spec.sample_rate.get()) * consts::FADE_IN_DURATION_MS) / 1000;
    computed.max(1) as usize
}

fn silence_params(threshold_db: f32, min_trim_frames: u64) -> SilenceTrimParams {
    SilenceTrimParams {
        threshold_db,
        min_trim_frames,
        scan_window_frames: 4096,
        trim_trailing: false,
    }
}

fn collect_pcm(out: &[AudioChunk]) -> Vec<f32> {
    out.iter().flat_map(|c| c.samples.to_vec()).collect()
}

#[kithara::test]
fn leading_trim_updates_offset_and_timestamp(trim_ramp: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from(GaplessInfo {
        leading_frames: 576,
        trailing_frames: 0,
    });

    let mut ready = trimmer.push(chunk(spec, 0, 1024, &trim_ramp));
    assert_eq!(ready.len(), 1);
    let out = ready.remove(0);
    assert_eq!(out.frames(), 448);
    assert_eq!(out.meta.frame_offset, 576);
    assert_eq!(out.meta.timestamp, Duration::from_millis(12));
    assert_eq!(out.samples[0], 576.0);
}

#[kithara::test]
fn leading_trim_can_consume_multiple_chunks(trim_ramp: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from(GaplessInfo {
        leading_frames: 2400,
        trailing_frames: 0,
    });

    assert!(trimmer.push(chunk(spec, 0, 1024, &trim_ramp)).is_empty());
    assert!(trimmer.push(chunk(spec, 1024, 1024, &trim_ramp)).is_empty());

    let mut ready = trimmer.push(chunk(spec, 2048, 1024, &trim_ramp));
    assert_eq!(ready.len(), 1);
    let out = ready.remove(0);
    assert_eq!(out.frames(), 672);
    assert_eq!(out.meta.frame_offset, 2400);
    assert_eq!(out.samples[0], 352.0);
}

#[kithara::test]
fn tail_compensation_reduces_fixed_trailing_trim_by_one_frame(trim_ramp: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from(GaplessInfo {
        leading_frames: 0,
        trailing_frames: 2,
    })
    .with_tail_compensation(Some(GaplessTailCompensation::new(5)));

    let mut output = super::GaplessOutput::new();
    output.extend(trimmer.push(chunk(spec, 0, 2, &trim_ramp)));
    output.extend(trimmer.push(chunk(spec, 2, 2, &trim_ramp)));
    output.extend(trimmer.flush());

    assert_eq!(output.len(), 2);
    assert_eq!(output.iter().map(AudioChunk::frames).sum::<usize>(), 3);
    assert_eq!(collect_pcm(&output), vec![0.0, 1.0, 0.0]);
}

#[kithara::test]
fn tail_compensation_is_identity_without_deficit(trim_ramp: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from(GaplessInfo {
        leading_frames: 0,
        trailing_frames: 2,
    })
    .with_tail_compensation(Some(GaplessTailCompensation::new(4)));

    let mut output = super::GaplessOutput::new();
    output.extend(trimmer.push(chunk(spec, 0, 2, &trim_ramp)));
    output.extend(trimmer.push(chunk(spec, 2, 2, &trim_ramp)));
    output.extend(trimmer.flush());

    assert_eq!(output.len(), 1);
    assert_eq!(output.iter().map(AudioChunk::frames).sum::<usize>(), 2);
    assert_eq!(collect_pcm(&output), vec![0.0, 1.0]);
}

#[kithara::test]
fn trailing_trim_buffers_until_flush(trim_ramp: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from(GaplessInfo {
        leading_frames: 0,
        trailing_frames: 64,
    });

    assert!(trimmer.push(chunk(spec, 0, 32, &trim_ramp)).is_empty());

    let mut ready = trimmer.push(chunk(spec, 32, 64, &trim_ramp));
    assert_eq!(ready.len(), 1);
    assert_eq!(ready.remove(0).frames(), 32);

    let ready = trimmer.flush();
    assert!(ready.is_empty());
}

#[kithara::test]
fn trailing_trim_drops_tail_on_flush(trim_ramp: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from(GaplessInfo {
        leading_frames: 0,
        trailing_frames: 2_048,
    });

    assert!(trimmer.push(chunk(spec, 0, 1_024, &trim_ramp)).is_empty());
    assert!(
        trimmer
            .push(chunk(spec, 1_024, 1_024, &trim_ramp))
            .is_empty()
    );
    assert_eq!(trimmer.push(chunk(spec, 2_048, 1_024, &trim_ramp)).len(), 1);

    let ready = trimmer.flush();
    assert!(ready.is_empty());
}

#[kithara::test]
fn trailing_trim_handles_more_than_inline_tail_chunks(trim_ramp: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from(GaplessInfo {
        leading_frames: 0,
        trailing_frames: 8,
    });
    let mut output = super::GaplessOutput::new();

    for frame in 0..8 {
        assert!(trimmer.push(chunk(spec, frame, 1, &trim_ramp)).is_empty());
    }

    output.extend(trimmer.push(chunk(spec, 8, 1, &trim_ramp)));
    output.extend(trimmer.flush());

    assert_eq!(output.len(), 1);
    let out = output.remove(0);
    assert_eq!(out.meta.frame_offset, 0);
    assert_eq!(out.frames(), 1);
}

#[kithara::test]
fn disabled_trimmer_passes_through(trim_ramp: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::disabled();
    let mut ready = trimmer.push(chunk(spec, 0, 128, &trim_ramp));
    assert_eq!(ready.len(), 1);
    assert_eq!(ready.remove(0).frames(), 128);
    assert!(trimmer.flush().is_empty());
}

#[kithara::test]
fn notify_seek_resets_leading_only(trim_ramp: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from(GaplessInfo {
        leading_frames: 128,
        trailing_frames: 64,
    });

    assert!(trimmer.push(chunk(spec, 0, 64, &trim_ramp)).is_empty());
    trimmer.notify_seek(&DropChunks);

    assert!(trimmer.push(chunk(spec, 64, 128, &trim_ramp)).is_empty());

    let mut ready = trimmer.flush();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready.remove(0).frames(), 64);
}

#[kithara::test]
fn codec_priming_with_zero_frames_is_disabled(trim_ramp: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::codec_priming(0, spec.sample_rate.get());
    let mut ready = trimmer.push(chunk(spec, 0, 64, &trim_ramp));
    assert_eq!(ready.len(), 1);
    assert_eq!(ready.remove(0).frames(), 64);
}

#[kithara::test]
fn codec_priming_drops_leading_frames_and_fades_in(
    trim_codec_priming_drops_leading_frames_and_fades_in: Vec<f32>,
) {
    let spec = mono_spec();
    let trim = 100u64;
    let trim_len = usize::try_from(trim).expect("BUG: test trim fits in usize");
    let total_frames = trim_len + fade_frames_for(spec) + 32;
    let pcm = trim_codec_priming_drops_leading_frames_and_fades_in;
    let mut trimmer = GaplessTrimmer::codec_priming(trim, spec.sample_rate.get());

    let ready = trimmer.push(custom_chunk(spec, 0, pcm));
    let pcm_out = collect_pcm(&ready);
    assert_eq!(pcm_out.len(), total_frames - trim_len);

    let fade_len = fade_frames_for(spec);
    assert!(
        pcm_out[0].abs() < 0.05,
        "first sample {} not soft",
        pcm_out[0]
    );
    for window in pcm_out[..fade_len].windows(2) {
        assert!(
            window[1] >= window[0] - 1e-6,
            "fade-in must be monotonically increasing: {:?}",
            window
        );
    }
    for &sample in &pcm_out[fade_len..] {
        assert!(
            (sample - 1.0).abs() < 1e-5,
            "post-fade sample {sample} != 1.0"
        );
    }
}

#[kithara::test]
fn codec_priming_metadata_takes_precedence_when_combined(
    trim_codec_priming_metadata_takes_precedence_when_combined: Vec<f32>,
) {
    let spec = mono_spec();
    let metadata_trimmer = GaplessTrimmer::from(GaplessInfo {
        leading_frames: 50,
        trailing_frames: 0,
    });
    let codec_trimmer = GaplessTrimmer::codec_priming(50, spec.sample_rate.get());
    let pcm = trim_codec_priming_metadata_takes_precedence_when_combined;

    let mut from_info = metadata_trimmer;
    let from_info_out = collect_pcm(&from_info.push(custom_chunk(spec, 0, pcm.clone())));
    assert_eq!(from_info_out[0], 0.5);

    let mut from_codec = codec_trimmer;
    let from_codec_out = collect_pcm(&from_codec.push(custom_chunk(spec, 0, pcm)));
    assert!(from_codec_out[0].abs() < 0.5 * 0.1);
}

#[kithara::test]
fn silence_trim_below_threshold_is_trimmed(trim_silence_trim_below_threshold_is_trimmed: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    let pcm = trim_silence_trim_below_threshold_is_trimmed;
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), 100);
    assert!(pcm_out[0].abs() < 0.05, "fade-in must soften the boundary");
}

#[kithara::test]
fn silence_trim_above_threshold_preserves_audio(
    trim_silence_trim_above_threshold_preserves_audio: Vec<f32>,
) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    let pcm = trim_silence_trim_above_threshold_preserves_audio;
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), 300);
    assert!(pcm_out.iter().all(|s| (*s - 3.16e-3).abs() < 1e-7));
}

#[kithara::test]
fn silence_trim_preserves_quiet_intro_below_threshold_then_above(
    trim_silence_trim_preserves_quiet_intro_below_threshold_then_above: Vec<f32>,
) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    let pcm = trim_silence_trim_preserves_quiet_intro_below_threshold_then_above;
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), 200);
    let fade_len = fade_frames_for(spec).min(pcm_out.len());
    for &sample in &pcm_out[fade_len..] {
        assert!((sample - 0.001_8).abs() < 1e-7);
    }
}

#[kithara::test]
fn silence_trim_min_frames_boundary_under_min(
    trim_silence_trim_min_frames_boundary_under_min: Vec<f32>,
) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    let pcm = trim_silence_trim_min_frames_boundary_under_min;
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), 95);
    assert_eq!(pcm_out[0], 0.0);
}

#[kithara::test]
fn silence_trim_min_frames_boundary_at_min(trim_silence_trim_min_frames_boundary_at_min: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    let pcm = trim_silence_trim_min_frames_boundary_at_min;
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), 64);
    assert!(pcm_out[0].abs() < 0.05);
}

#[kithara::test]
fn silence_trim_scan_window_exhausted_preserves_audio(
    trim_silence_trim_scan_window_exhausted_preserves_audio: Vec<f32>,
) {
    let spec = mono_spec();
    let params = SilenceTrimParams {
        threshold_db: 60.0,
        min_trim_frames: 32,
        scan_window_frames: 256,
        trim_trailing: false,
    };
    let mut trimmer = GaplessTrimmer::silence_trim(params);

    let pcm = trim_silence_trim_scan_window_exhausted_preserves_audio;
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), 300);
    assert!(pcm_out.iter().all(|s| *s == 0.0));
}

#[kithara::test]
fn silence_trim_no_op_with_immediate_content(
    trim_silence_trim_no_op_with_immediate_content: Vec<f32>,
) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    let pcm = trim_silence_trim_no_op_with_immediate_content;
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), 256);
    assert!(pcm_out.iter().all(|s| (*s - 0.5).abs() < 1e-7));
}

#[kithara::test]
fn silence_trim_trailing_disabled_by_default(
    trim_silence_trim_trailing_disabled_by_default: Vec<f32>,
) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    let pcm = trim_silence_trim_trailing_disabled_by_default;
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), 128);
}

#[kithara::test]
fn silence_trim_trailing_enabled(trim_silence_trim_trailing_enabled: Vec<f32>) {
    let spec = mono_spec();
    let params = SilenceTrimParams {
        threshold_db: 60.0,
        min_trim_frames: 32,
        scan_window_frames: 4096,
        trim_trailing: true,
    };
    let mut trimmer = GaplessTrimmer::silence_trim(params);

    let audible_frames = 256;
    let pcm = trim_silence_trim_trailing_enabled;
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), audible_frames);

    let fade_frames =
        (u64::from(spec.sample_rate.get()) * consts::FADE_OUT_DURATION_MS / 1000).max(1) as usize;
    let untouched = pcm_out.len().saturating_sub(fade_frames);
    for &sample in &pcm_out[..untouched] {
        assert!(
            (sample - 0.5).abs() < 1e-7,
            "untouched sample changed: {sample}"
        );
    }
    for window in pcm_out[untouched..].windows(2) {
        assert!(
            window[1] <= window[0] + 1e-6,
            "fade-out must be monotonically non-increasing: {window:?}"
        );
    }
    assert!(
        pcm_out
            .last()
            .copied()
            .is_some_and(|sample| sample.abs() < 0.05),
        "last sample must be near zero after fade-out"
    );
}

#[kithara::test]
fn silence_trim_trailing_window_rms_ignores_zero_crossings_in_audible_signal(
    trim_trailing_sine: Vec<f32>,
) {
    let spec = mono_spec();
    let params = SilenceTrimParams {
        threshold_db: 60.0,
        min_trim_frames: 32,
        scan_window_frames: 4096,
        trim_trailing: true,
    };
    let mut trimmer = GaplessTrimmer::silence_trim(params);

    let sine_frames: u32 = 4_800;
    let pcm = trim_trailing_sine;
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert_eq!(pcm_out.len(), sine_frames as usize);

    let fade_frames =
        (u64::from(spec.sample_rate.get()) * consts::FADE_OUT_DURATION_MS / 1000).max(1) as usize;
    let pre_fade_end = pcm_out.len().saturating_sub(fade_frames);
    let pre_fade = &pcm_out[..pre_fade_end];
    let pre_fade_len = u32::try_from(pre_fade.len()).expect("BUG: pre-fade window fits in u32");
    let sum_sq: f64 = pre_fade.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    let rms_f64: f64 = (sum_sq / f64::from(pre_fade_len)).sqrt();
    let rms: f32 = num_traits::cast::AsPrimitive::<f32>::as_(rms_f64);
    assert!(
        (rms - 0.5 / std::f32::consts::SQRT_2).abs() < 0.02,
        "RMS of surviving signal should match a 0.5 sine: got {rms}"
    );
}

#[kithara::test]
fn silence_trim_seek_disables_leading_only(trim_silence: Vec<f32>, trim_seek: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    assert!(
        trimmer
            .push(silent_chunk(spec, 0, 128, &trim_silence))
            .is_empty()
    );
    trimmer.notify_seek(&DropChunks);

    assert!(trimmer.push(custom_chunk(spec, 128, trim_seek)).is_empty());

    let mut flushed = trimmer.flush();
    assert_eq!(flushed.len(), 1);
    let out = flushed.remove(0);
    assert_eq!(out.meta.frame_offset, 128);
    assert_eq!(&out.samples[..], &[0.0, 0.0, 3.0, 4.0]);
}

#[kithara::test]
fn silence_trim_preserves_all_silence_track(trim_silence: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    assert!(
        trimmer
            .push(silent_chunk(spec, 0, 128, &trim_silence))
            .is_empty()
    );

    let mut flushed = trimmer.flush();
    assert_eq!(flushed.len(), 1);
    let out = flushed.remove(0);
    assert_eq!(out.frames(), 128);
    assert!(out.samples.iter().all(|sample| *sample == 0.0));
}

#[kithara::test]
fn silence_trim_respects_multi_channel_threshold(
    trim_stereo_silence: Vec<f32>,
    trim_stereo_quiet: Vec<f32>,
) {
    let spec = stereo_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    assert!(
        trimmer
            .push(custom_chunk(spec, 0, trim_stereo_silence))
            .is_empty()
    );
    assert!(
        trimmer
            .push(custom_chunk(spec, 32, trim_stereo_quiet))
            .is_empty()
    );

    let mut flushed = trimmer.flush();
    assert_eq!(flushed.len(), 1);
    let out = flushed.remove(0);
    assert_eq!(out.meta.frame_offset, 32);
    assert_eq!(out.frames(), 1);
    assert!(out.samples[1].abs() < 2.0e-3);
}

#[kithara::test]
fn silence_trim_does_not_introduce_click_at_boundary(
    trim_silence_trim_does_not_introduce_click_at_boundary: Vec<f32>,
) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::silence_trim(silence_params(60.0, 32));

    let pcm = trim_silence_trim_does_not_introduce_click_at_boundary;
    assert!(trimmer.push(custom_chunk(spec, 0, pcm)).is_empty());

    let flushed = trimmer.flush();
    let pcm_out = collect_pcm(&flushed);
    assert!(
        pcm_out[0].abs() < 0.1,
        "boundary sample {} too loud",
        pcm_out[0]
    );
}
