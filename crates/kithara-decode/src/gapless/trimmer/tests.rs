use std::num::NonZeroU32;

use kithara_bufpool::SampleBuffer;
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_test_fixtures::unit_fixtures::{
    trim_codec_priming_drops_leading_frames_and_fades_in,
    trim_codec_priming_metadata_takes_precedence_when_combined, trim_ramp,
};
use kithara_test_utils::kithara;

use super::GaplessTrimmer;
use crate::{GaplessInfo, GaplessTailCompensation, consts, test_pools::pools};

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
        sample_buffer(pcm),
    )
}

fn custom_chunk(spec: AudioSpec, frame_offset: u64, pcm: &[f32]) -> AudioChunk {
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            frame_offset,
            ..Default::default()
        },
        sample_buffer(pcm),
    )
}

fn mono_spec() -> AudioSpec {
    AudioSpec::new(1, NonZeroU32::new(48_000).expect("test rate"))
}

fn fade_frames_for(spec: AudioSpec) -> usize {
    let computed = (u64::from(spec.sample_rate.get()) * consts::FADE_IN_DURATION_MS) / 1000;
    computed.max(1) as usize
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

    let mut ready = trimmer
        .push(chunk(spec, 0, 1024, &trim_ramp))
        .expect("valid gapless PCM");
    assert_eq!(ready.len(), 1);
    let out = ready.remove(0);
    assert_eq!(out.frames(), 448);
    assert_eq!(out.meta.frame_offset, 576);
    assert_eq!(out.meta.timestamp, Duration::from_millis(12));
    assert_eq!(out.samples[0], 576.0);

    let spec = AudioSpec::new(1, NonZeroU32::new(44_100).expect("fractional frame rate"));
    for phase in [Duration::ZERO, Duration::from_micros(250)] {
        let mut whole = GaplessTrimmer::from(GaplessInfo {
            leading_frames: 1_015,
            trailing_frames: 0,
        });
        let mut input = chunk(spec, 0, 1_200, &trim_ramp);
        input.meta.timestamp = phase;
        let whole = whole.push(input).expect("valid gapless PCM").remove(0);

        let mut split = GaplessTrimmer::from(GaplessInfo {
            leading_frames: 1_015,
            trailing_frames: 0,
        });
        let mut head = chunk(spec, 0, 941, &trim_ramp);
        head.meta.timestamp = phase;
        assert!(split.push(head).expect("valid gapless PCM").is_empty());
        let mut tail = chunk(spec, 941, 259, &trim_ramp[941..]);
        tail.meta.timestamp = phase + spec.duration_for(941).expect("partition timestamp");
        let split = split.push(tail).expect("valid gapless PCM").remove(0);

        assert_eq!(whole.meta.frame_offset, 1_015);
        assert_eq!(split.meta.frame_offset, whole.meta.frame_offset);
        assert_eq!(whole.frames(), 185);
        assert_eq!(split.frames(), whole.frames());
        assert_eq!(whole.samples.as_ref(), &trim_ramp[1_015..1_200]);
        assert_eq!(split.samples.as_ref(), whole.samples.as_ref());
        assert_eq!(
            whole.meta.timestamp,
            phase + Duration::from_nanos(23_015_873),
        );
        assert_eq!(split.meta.timestamp, whole.meta.timestamp);
    }
    for (span_frames, meta_frames) in [(1, 128), (128, 1)] {
        let mut trimmer = GaplessTrimmer::from(GaplessInfo {
            leading_frames: 2,
            trailing_frames: 2,
        });
        let mapped = kithara_signal::SourceSpan::new(0, span_frames, spec.sample_rate, span_frames)
            .expect("valid source mapping with mismatched public PCM extent");
        let mut invalid = chunk(spec, 0, 128, &trim_ramp);
        invalid.meta.frames = meta_frames;
        invalid.meta.source_span = Some(mapped);
        assert!(matches!(
            trimmer.push(invalid),
            Err(crate::DecodeError::InvalidData { .. })
        ));
        let mut valid = chunk(spec, 0, 128, &trim_ramp);
        valid.meta.frames = 128;
        valid.meta.source_span = Some(
            kithara_signal::SourceSpan::new(0, 128, spec.sample_rate, 128)
                .expect("valid mapped PCM"),
        );
        assert!(
            trimmer
                .push(valid)
                .expect("invalid input leaves leading state intact")
                .is_empty()
        );
        let output = trimmer.flush().expect("valid mapped tail");
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].meta.frame_offset, 2);
        assert_eq!(output[0].frames(), 124);
        assert_eq!(output[0].samples.as_ref(), &trim_ramp[2..126]);
        assert_eq!(
            output[0]
                .meta
                .source_span
                .expect("sliced mapping")
                .output_frames(),
            124
        );
    }
}

#[kithara::test]
fn leading_trim_can_consume_multiple_chunks(trim_ramp: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from(GaplessInfo {
        leading_frames: 2400,
        trailing_frames: 0,
    });

    assert!(
        trimmer
            .push(chunk(spec, 0, 1024, &trim_ramp))
            .expect("valid gapless PCM")
            .is_empty()
    );
    assert!(
        trimmer
            .push(chunk(spec, 1024, 1024, &trim_ramp))
            .expect("valid gapless PCM")
            .is_empty()
    );

    let mut ready = trimmer
        .push(chunk(spec, 2048, 1024, &trim_ramp))
        .expect("valid gapless PCM");
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
    output.extend(
        trimmer
            .push(chunk(spec, 0, 2, &trim_ramp))
            .expect("valid gapless PCM"),
    );
    output.extend(
        trimmer
            .push(chunk(spec, 2, 2, &trim_ramp))
            .expect("valid gapless PCM"),
    );
    output.extend(trimmer.flush().expect("valid gapless PCM"));

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
    output.extend(
        trimmer
            .push(chunk(spec, 0, 2, &trim_ramp))
            .expect("valid gapless PCM"),
    );
    output.extend(
        trimmer
            .push(chunk(spec, 2, 2, &trim_ramp))
            .expect("valid gapless PCM"),
    );
    output.extend(trimmer.flush().expect("valid gapless PCM"));

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

    assert!(
        trimmer
            .push(chunk(spec, 0, 32, &trim_ramp))
            .expect("valid gapless PCM")
            .is_empty()
    );

    let mut ready = trimmer
        .push(chunk(spec, 32, 64, &trim_ramp))
        .expect("valid gapless PCM");
    assert_eq!(ready.len(), 1);
    assert_eq!(ready.remove(0).frames(), 32);

    let ready = trimmer.flush().expect("valid gapless PCM");
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
        assert!(
            trimmer
                .push(chunk(spec, frame, 1, &trim_ramp))
                .expect("valid gapless PCM")
                .is_empty()
        );
    }

    output.extend(
        trimmer
            .push(chunk(spec, 8, 1, &trim_ramp))
            .expect("valid gapless PCM"),
    );
    output.extend(trimmer.flush().expect("valid gapless PCM"));

    assert_eq!(output.len(), 1);
    let out = output.remove(0);
    assert_eq!(out.meta.frame_offset, 0);
    assert_eq!(out.frames(), 1);
}

#[kithara::test]
fn disabled_trimmer_passes_through(trim_ramp: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::disabled();
    let mut ready = trimmer
        .push(chunk(spec, 0, 128, &trim_ramp))
        .expect("valid gapless PCM");
    assert_eq!(ready.len(), 1);
    assert_eq!(ready.remove(0).frames(), 128);
    assert!(trimmer.flush().expect("valid gapless PCM").is_empty());
}

#[kithara::test]
fn notify_seek_resets_leading_only(trim_ramp: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::from(GaplessInfo {
        leading_frames: 128,
        trailing_frames: 64,
    });

    assert!(
        trimmer
            .push(chunk(spec, 0, 64, &trim_ramp))
            .expect("valid gapless PCM")
            .is_empty()
    );
    trimmer.notify_seek();

    assert!(
        trimmer
            .push(chunk(spec, 64, 128, &trim_ramp))
            .expect("valid gapless PCM")
            .is_empty()
    );

    let mut ready = trimmer.flush().expect("valid gapless PCM");
    assert_eq!(ready.len(), 1);
    assert_eq!(ready.remove(0).frames(), 64);
}

#[kithara::test]
fn codec_priming_with_zero_frames_is_disabled(trim_ramp: Vec<f32>) {
    let spec = mono_spec();
    let mut trimmer = GaplessTrimmer::codec_priming(0, spec.sample_rate.get());
    let mut ready = trimmer
        .push(chunk(spec, 0, 64, &trim_ramp))
        .expect("valid gapless PCM");
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

    let ready = trimmer
        .push(custom_chunk(spec, 0, &pcm))
        .expect("valid gapless PCM");
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
    let from_info_out = collect_pcm(
        &from_info
            .push(custom_chunk(spec, 0, &pcm))
            .expect("valid gapless PCM"),
    );
    assert_eq!(from_info_out[0], 0.5);

    let mut from_codec = codec_trimmer;
    let from_codec_out = collect_pcm(
        &from_codec
            .push(custom_chunk(spec, 0, &pcm))
            .expect("valid gapless PCM"),
    );
    assert!(from_codec_out[0].abs() < 0.5 * 0.1);
}
