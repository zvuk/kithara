pub(super) use std::num::NonZeroU32;

pub(super) use kithara_signal::{AudioSpec, FrameCount, SampleCount, SourceSpan};
pub(super) use kithara_test_utils::kithara;

use super::*;
pub(super) use crate::worker::packet_tests::chunk;

#[kithara::test]
#[case::samples(false)]
#[case::mark(true)]
fn an_adopted_segment_never_uses_a_held_obsolete_packet(#[case] check_mark: bool) {
    use crate::{
        test_pools::pools,
        worker::packet_tests::{PacketRing, chunk},
    };

    let spec = AudioSpec::new(1, NonZeroU32::new(44_100).expect("rate"));
    let old = SegmentId::FIRST;
    let intermediate = old.next();
    let current = intermediate.next();
    let mut ring = PacketRing::new(spec, Duration::from_secs(1), 2);
    let mut receiver = ring.receiver.take().expect("receiver");
    receiver
        .recycle(PcmPacket::Chunk(Box::new(chunk(spec, old, 0, 0, &[1.0]))))
        .expect("first reverse entry");
    ring.push(PcmPacket::Chunk(Box::new(chunk(
        spec, old, 10, 10, &[1.0; 8],
    ))));
    ring.push(PcmPacket::Chunk(Box::new(chunk(
        spec,
        intermediate,
        18,
        18,
        &[1.0; 8],
    ))));
    let mut resource =
        PlayerResource::new(PcmConsumer::new(receiver), Arc::from("segments"), &pools())
            .expect("resource");
    resource.select_segment(intermediate);
    let mut budget = 1;
    resource.refresh_mark(&mut budget);
    assert_eq!(budget, 0, "this block spent its recycle budget");
    let mut previous_left = [0.0; 1];
    let mut previous_right = [0.0; 1];
    resource.read(
        &mut [&mut previous_left, &mut previous_right],
        0..1,
        &mut budget,
    );
    assert_eq!(
        previous_left,
        [1.0],
        "the intermediate packet is held before the next Adopt"
    );
    ring.push(PcmPacket::Chunk(Box::new(chunk(
        spec, current, 100, 1000, &[2.0; 8],
    ))));
    ring.push(PcmPacket::Chunk(Box::new(chunk(
        spec, current, 108, 1008, &[2.0; 8],
    ))));
    assert!(
        resource
            .consumer
            .get_mut()
            .receiver
            .recycle(PcmPacket::Chunk(Box::new(chunk(spec, old, 0, 0, &[1.0]))))
            .is_err(),
        "the reverse ring is full"
    );
    resource.select_segment(current);
    let mut left = [0.0; 4];
    let mut right = [0.0; 4];
    let adopted_at = SessionFrame::new(700);
    resource.read(&mut [&mut left, &mut right], 0..4, &mut budget);
    if check_mark {
        assert_eq!(
            resource.mark(adopted_at),
            None,
            "an obsolete held packet cannot establish the adopted segment's frame or position"
        );
    } else {
        assert_eq!(
            left, [0.0; 4],
            "no old-segment PCM may reach the mix after Adopt, even under full rings and exhausted budget"
        );
        assert_eq!(right, left);
    }
    while ring.returned().is_some() {}
    budget = 2;
    resource.refresh_mark(&mut budget);
    resource.read(&mut [&mut left, &mut right], 0..4, &mut budget);
    assert_eq!(left, [2.0; 4]);
    assert_eq!(
        resource.mark(SessionFrame::new(704)),
        Some(SlotMark {
            session: SessionFrame::new(704),
            lane: LaneFrame {
                segment: current,
                frame: 104
            },
            position: spec.duration_for(1004).expect("new segment position"),
        })
    );
}

#[kithara::test]
#[case(44_100, 8_820)]
#[case(48_000, 9_600)]
#[case(96_000, 19_200)]
fn a_pcm_packet_holds_200ms_of_frames(#[case] sample_rate: u32, #[case] expected: usize) {
    let spec = AudioSpec::new(2, NonZeroU32::new(sample_rate).expect("rate"));
    let duration = Duration::from_millis(200);
    let frames = spec.frames_for(duration).expect("200 ms geometry");
    let samples = spec.sample_count(frames).expect("stereo geometry");
    let packet = chunk(spec, SegmentId::FIRST, 0, 0, &vec![1.0; samples.get()]);
    assert_eq!(FrameCount::new(packet.frames()), FrameCount::new(expected));
    assert_eq!(packet.meta.end_timestamp, duration);
}

#[kithara::test]
fn an_interleaved_length_is_not_a_frame_count() {
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("test rate is non-zero"));
    let frames = FrameCount::new(9_600);
    assert_eq!(
        spec.sample_count(frames),
        Ok(SampleCount::new(frames.get() * 2))
    );
}

#[kithara::test]
fn partial_scratch_consumption_preserves_the_render_revision() {
    let rate = NonZeroU32::new(48_000).expect("fixture sample rate is non-zero");
    let source = SourceSpan::new(100, 130, rate, 10).map(|span| span.with_render_revision(7));
    let span = source.expect("source span");

    assert_eq!(
        span.for_output_range(0..4),
        SourceSpan::new(100, 112, rate, 4).map(|span| span.with_render_revision(7))
    );
    assert_eq!(
        span.for_output_range(4..10).map(SourceSpan::start),
        Some(112)
    );
    assert_eq!(
        span.for_output_range(4..10),
        SourceSpan::new(112, 130, rate, 6).map(|span| span.with_render_revision(7))
    );
}

#[kithara::test]
fn partial_source_frontier_is_independent_of_callback_partitions() {
    let rate = NonZeroU32::new(48_000).expect("sample rate");
    let mapping = std::num::NonZeroU64::new(3);
    let source = SourceSpan::new(100, 292, rate, 128)
        .map(|span| span.with_render_revision(7).with_mapping_revision(mapping));
    let whole = source.expect("source span");
    let expected = whole.for_output_range(0..2).expect("two output frames");
    let split = whole.for_output_range(1..128).expect("first output frame");
    let actual = split.for_output_range(0..1).expect("second output frame");
    assert_eq!(actual.end(), expected.end());
    assert_eq!(actual.end(), 103);
    assert_eq!(actual.render_revision(), 7);
    assert_eq!(actual.mapping_revision(), mapping);
}
#[kithara::test]
fn zero_source_advance_keeps_mapping_identity_until_pcm_is_consumed() {
    let rate = NonZeroU32::new(48_000).expect("rate");
    let mapping = std::num::NonZeroU64::new(2);
    let source = SourceSpan::new(41, 41, rate, 32)
        .map(|span| span.with_render_revision(7).with_mapping_revision(mapping));
    let window = source.expect("source span");
    assert_eq!(
        window.for_output_range(0..16),
        source.and_then(|span| span.for_output_range(0..16))
    );
    assert_eq!(
        window.for_output_range(16..32),
        source.and_then(|span| span.for_output_range(16..32))
    );
    assert_eq!(
        window.for_output_range(16..32),
        source.and_then(|span| span.for_output_range(0..16))
    );
    assert_eq!(
        window
            .for_output_range(32..32)
            .expect("exhausted window")
            .output_frames(),
        0
    );
}
#[kithara::test]
#[case::zero_origin(0)]
#[case::nonzero_origin(100)]
fn nested_audio_source_window_preserves_original_rounding(#[case] origin: u64) {
    let rate = NonZeroU32::new(48_000).expect("rate");
    let mapping = std::num::NonZeroU64::new(3);
    let source = SourceSpan::new(origin, origin + 192, rate, 128)
        .expect("source span")
        .with_render_revision(7)
        .with_mapping_revision(mapping);
    let audio_read = source.for_output_range(0..127).expect("Audio partial read");
    assert_eq!(audio_read.end(), origin + 190);
    let consumed = audio_read
        .for_output_range(0..2)
        .expect("Play partial consumption");
    assert_eq!(consumed.end(), origin + 3);
    assert_eq!(
        consumed,
        source.for_output_range(0..2).expect("direct slice")
    );
    assert_eq!(consumed.render_revision(), 7);
    assert_eq!(consumed.mapping_revision(), mapping);
}
