use std::num::NonZeroU32;

use kithara_decode::BlenderProfile;
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use kithara_test_fixtures::unit_fixtures::{
    blend_identity, blend_incoming, blend_join_frame, blend_multichannel, blend_outgoing,
    blend_outgoing_constant, blend_signed_frame,
};
use kithara_test_utils::kithara;

use super::GaplessBlender;
use crate::test_pools::{Pools, pools, sample_buffer};

fn spec(channels: u16, sample_rate: u32) -> AudioSpec {
    AudioSpec::new(
        channels,
        NonZeroU32::new(sample_rate).expect("test rate must be non-zero"),
    )
}

fn chunk(pools: &Pools, spec: AudioSpec, samples: &[f32]) -> AudioChunk {
    let frames = samples.len() / usize::from(spec.channels);
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            end_timestamp: Duration::from_millis(42),
            timestamp: Duration::from_millis(21),
            segment_index: Some(7),
            source_byte_offset: Some(1_024),
            variant_index: Some(2),
            frames: u32::try_from(frames).expect("fixture frame count"),
            segment: kithara_signal::SegmentId::FIRST,
            render_revision: 13,
            mapping_revision: std::num::NonZeroU64::new(17),
            frame_offset: 9_876,
            source_bytes: 512,
            ..Default::default()
        },
        sample_buffer(pools, samples),
    )
}

fn blender(pools: &Pools, profile: BlenderProfile) -> GaplessBlender {
    GaplessBlender::new(profile, pools).expect("blender scratch fits test pools")
}

#[kithara::test]
fn single_input_blender_is_bit_exact(blend_identity: Vec<f32>) {
    let pools = pools();
    let spec = spec(2, 48_000);
    let input = chunk(&pools, spec, &blend_identity);
    let input_ptr = input.samples.as_ptr();
    let input_meta = input.meta;
    let input_bits = input
        .samples
        .iter()
        .map(|sample| sample.to_bits())
        .collect::<Vec<_>>();
    let mut blender = blender(&pools, BlenderProfile::new(spec));

    let output = blender.process_active(input);

    assert_eq!(output.samples.as_ptr(), input_ptr);
    assert_eq!(output.meta, input_meta);
    assert_eq!(
        output
            .samples
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        input_bits
    );
    assert!(output.samples.iter().all(|sample| sample.is_finite()));
}

#[kithara::test]
fn replacing_active_profile_accepts_the_new_spec(blend_multichannel: Vec<f32>) {
    let pools = pools();
    let initial = spec(2, 44_100);
    let replacement = spec(6, 48_000);
    let mut blender = blender(&pools, BlenderProfile::new(initial));

    blender
        .prepare_active(BlenderProfile::new(replacement))
        .expect("replacement scratch fits test pools");
    let capacities_before = blender.buffer_capacities();
    let prepared_capacity = blender.buffer_capacities().1;
    blender.replace_active(BlenderProfile::new(replacement));
    let capacities_after = blender.buffer_capacities();

    let output = blender.process_active(chunk(&pools, replacement, &blend_multichannel));

    assert_eq!(output.spec(), replacement);
    assert_eq!(capacities_after.0, prepared_capacity);
    assert_eq!(
        [capacities_after.0, capacities_after.1].into_iter().min(),
        [capacities_before.0, capacities_before.1].into_iter().min()
    );
    assert_eq!(
        [capacities_after.0, capacities_after.1].into_iter().max(),
        [capacities_before.0, capacities_before.1].into_iter().max()
    );
}

#[kithara::test]
fn high_rate_join_uses_the_full_forty_milliseconds() {
    let pools = pools();
    let spec = spec(2, 384_000);
    let blender = blender(&pools, BlenderProfile::new(spec));

    assert_eq!(blender.join_frame_count(), 15_360);
}

#[kithara::test]
fn real_outgoing_pcm_is_blended_for_the_full_linear_join(
    blend_outgoing: Vec<f32>,
    blend_incoming: Vec<f32>,
) {
    const CHUNK_FRAMES: usize = 128;
    const JOIN_FRAMES: usize = 1_764;

    let pools = pools();
    let spec = spec(2, 44_100);
    let channels = usize::from(spec.channels);
    let outgoing = blend_outgoing
        .into_iter()
        .cycle()
        .take(JOIN_FRAMES.saturating_mul(channels))
        .collect::<Vec<_>>();
    let incoming = blend_incoming;
    let mut blender = blender(&pools, BlenderProfile::new(spec));
    blender
        .prepare_active(BlenderProfile::new(spec))
        .expect("join scratch fits test pools");
    assert!(blender.prepare_join(|tail| {
        tail.copy_from_slice(&outgoing);
        true
    }));
    blender.commit_join();

    let mut output = Vec::with_capacity(incoming.len());
    for (chunk_index, samples) in incoming.chunks(CHUNK_FRAMES * channels).enumerate() {
        let mut input = chunk(&pools, spec, samples);
        input.meta.frame_offset =
            u64::try_from(chunk_index.saturating_mul(CHUNK_FRAMES)).unwrap_or(u64::MAX);
        let input_meta = input.meta;

        let processed = blender.process_active(input);

        assert_eq!(processed.meta, input_meta);
        output.extend_from_slice(&processed.samples);
    }

    for frame in 0..JOIN_FRAMES {
        let incoming_gain = f32::from(u16::try_from(frame).expect("join frame fits u16"))
            / f32::from(u16::try_from(JOIN_FRAMES).expect("join length fits u16"));
        let outgoing_gain = 1.0 - incoming_gain;
        for channel in 0..channels {
            let sample = frame * channels + channel;
            let expected =
                outgoing[sample].mul_add(outgoing_gain, incoming[sample] * incoming_gain);
            assert_eq!(output[sample].to_bits(), expected.to_bits());
        }
    }
    for sample in JOIN_FRAMES * channels..incoming.len() {
        assert_eq!(output[sample].to_bits(), incoming[sample].to_bits());
    }
}

#[kithara::test]
fn reset_cancels_an_active_join(
    blend_outgoing_constant: Vec<f32>,
    blend_join_frame: Vec<f32>,
    blend_signed_frame: Vec<f32>,
) {
    let pools = pools();
    let spec = spec(2, 44_100);
    let mut blender = blender(&pools, BlenderProfile::new(spec));
    let outgoing = blend_outgoing_constant
        .into_iter()
        .cycle()
        .take(
            usize::try_from(blender.join_frame_count())
                .expect("join frame count fits usize")
                .saturating_mul(usize::from(spec.channels)),
        )
        .collect::<Vec<_>>();
    blender
        .prepare_active(BlenderProfile::new(spec))
        .expect("join scratch fits test pools");
    assert!(blender.prepare_join(|tail| {
        tail.copy_from_slice(&outgoing);
        true
    }));
    blender.commit_join();
    let joined = blender.process_active(chunk(&pools, spec, &blend_join_frame));
    assert_eq!(joined.samples[0].to_bits(), (-0.75_f32).to_bits());

    blender.reset();
    let input = chunk(&pools, spec, &blend_signed_frame);
    let input_bits = input
        .samples
        .iter()
        .map(|sample| sample.to_bits())
        .collect::<Vec<_>>();
    let output = blender.process_active(input);

    assert_eq!(
        output
            .samples
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        input_bits
    );
}
