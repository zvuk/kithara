use std::num::NonZeroU32;

use kithara_platform::sync::Arc;
use kithara_signal::SegmentId;
use kithara_stream::{PlayheadState, PlayheadWrite};
use kithara_test_utils::kithara;

pub(in crate::worker) use super::super::mock::packet_fixture;
use super::*;

#[kithara::test]
fn cache_coverage_updates_while_the_pcm_ring_is_full_and_paused() {
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("sample rate"));
    let mut ring = PacketRing::new(spec, Duration::from_secs(187), 1);
    ring.push(PcmPacket::Chunk(Box::new(chunk(
        spec,
        SegmentId::FIRST,
        0,
        0,
        &[0.5; 8],
    ))));
    assert!(!ring.playing());
    let receiver = ring.receiver.as_mut().expect("receiver");
    let playhead = Arc::new(PlayheadState::new());
    receiver.playhead = playhead.clone();
    receiver.set_position(Duration::from_secs(10));
    assert!(receiver.forward.is_full());
    assert_eq!(receiver.cached_span(), Duration::ZERO);

    playhead.set_cached(1, 4);
    assert_eq!(receiver.cached_span(), Duration::ZERO);

    playhead.set_duration(Some(Duration::from_secs(187)));
    assert_eq!(receiver.cached_span(), Duration::from_millis(46_750));

    playhead.set_cached(4, 4);
    assert_eq!(receiver.cached_span(), Duration::from_secs(187));

    playhead.set_cached(3, 0);
    assert_eq!(receiver.cached_span(), Duration::from_secs(187));
    assert_eq!(receiver.position(), Duration::from_secs(10));
    assert!(receiver.forward.is_full());
}

#[kithara::test]
fn terminal_source_endpoint_publishes_visible_duration_before_ring_admission() {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("sample rate"));
    let samples = [0.5; 128];
    let frames = 64;
    let origin = 1_105;
    let source_origin = spec.duration_for(origin).expect("source origin");
    let endpoint = spec.duration_for(origin + frames).expect("source endpoint");
    let visible = endpoint - source_origin;
    let mut ring = PacketRing::with_origin(spec, Duration::from_secs(60), 2, source_origin);
    ring.playhead.set_duration(None);
    let data = chunk(spec, SegmentId::FIRST, 0, origin, &samples);
    let span = data.meta.source_span;
    ring.push(PcmPacket::Chunk(Box::new(data)));
    assert_eq!(ring.receiver.as_ref().expect("receiver").duration(), None);
    let mut terminal = chunk(spec, SegmentId::FIRST, frames, origin + frames, &[]);
    terminal.meta.end_of_track = true;
    ring.push(PcmPacket::Chunk(Box::new(terminal)));
    assert!(!ring.playing());
    let receiver = ring.receiver.as_mut().expect("receiver");
    assert!(receiver.forward.is_full());
    assert_eq!(receiver.duration(), Some(visible));
    let PcmPacket::Chunk(data) = receiver.pop().expect("physical PCM") else {
        panic!("PCM packet");
    };
    assert_eq!(&data.samples[..], &samples);
    assert_eq!(u64::from(data.meta.frames), frames);
    assert_eq!(data.meta.source_span, span);
    assert_eq!(data.meta.frame_offset, origin);
    assert_eq!(data.meta.timestamp, source_origin);
    assert_eq!(receiver.duration(), Some(visible));
    let PcmPacket::Chunk(terminal) = receiver.pop().expect("terminal marker") else {
        panic!("terminal packet");
    };
    assert!(terminal.meta.end_of_track);
    assert!(terminal.samples.is_empty());
    assert_eq!(terminal.meta.timestamp, endpoint);
    assert_eq!(terminal.meta.end_timestamp, endpoint);
    assert_eq!(receiver.decoded_frontier(), endpoint);
    assert_eq!(receiver.duration(), Some(visible));
}
