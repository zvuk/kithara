#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, FrameCount, SegmentId, SourceSpan};
use kithara_test_utils::kithara;

use super::super::{PcmConsumer, PlayerResource, ReadOutcome};
use crate::{
    mock::pcm_fixture::{PcmFixture, chunk},
    test_pools::{pools, sample_buffer},
    worker::PcmPacket,
};

#[kithara::test(native, tokio)]
#[case(44_100, 8_820)]
#[case(48_000, 9_600)]
#[case(96_000, 19_200)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn scratch_holds_200ms_of_frames(#[case] sample_rate: u32, #[case] expected: usize) {
    hang_tick!();
    let mut fixture = PcmFixture::new(4, false).await;
    hang_reset!();
    let spec = AudioSpec::new(2, NonZeroU32::new(sample_rate).expect("fixture rate"));
    let duration = Duration::from_millis(200);
    let frames = spec.frames_for(duration).expect("200 ms input geometry");
    let samples = [0.25, -0.5].repeat(frames.get());
    let packet = AudioChunk::new(
        AudioChunkInfo {
            spec,
            frames: u32::try_from(frames.get()).expect("fixture frame count"),
            segment: SegmentId::FIRST,
            end_timestamp: duration,
            source_span: SourceSpan::new(
                0,
                frames.get() as u64,
                spec.sample_rate,
                frames.get() as u64,
            ),
            ..AudioChunkInfo::default()
        },
        sample_buffer(&pools(), &samples),
    );
    fixture
        .push(PcmPacket::Chunk(Box::new(packet)))
        .expect("200 ms packet fits the ring");
    let receiver = fixture.receiver.take().expect("packet receiver");
    let mut resource =
        PlayerResource::new(PcmConsumer::new(receiver), Arc::from("200ms"), &pools())
            .expect("packet-backed resource");
    let mut left = vec![0.0; expected];
    let mut right = vec![0.0; expected];
    let split = expected / 2;
    let mut budget = 4;
    let ReadOutcome::Full { frames: first } =
        resource.read(None, &mut [&mut left, &mut right], 0..split, &mut budget)
    else {
        panic!("the first read must consume the packet prefix")
    };
    assert_eq!(first, split);
    let ReadOutcome::Full { frames: second } = resource.read(
        None,
        &mut [&mut left, &mut right],
        split..expected,
        &mut budget,
    ) else {
        panic!("the second read must consume the held packet remainder")
    };
    assert_eq!(FrameCount::new(first + second), FrameCount::new(expected));
    assert_eq!(resource.lane.frame, expected as u64);
    assert_eq!(left, vec![0.25; expected]);
    assert_eq!(right, vec![-0.5; expected]);
}

#[kithara::test(native, tokio)]
async fn epoch_validator_keeps_matching_chunks() {
    let mut fixture = PcmFixture::new(4, false).await;
    let receiver = fixture.receiver.take().expect("packet receiver");
    let mut resource = PlayerResource::new(PcmConsumer::new(receiver), Arc::from("test"), &pools())
        .expect("player resource");
    let segment = SegmentId::FIRST.next();
    resource.select_segment(segment);
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(segment, &[1.0, 2.0, 3.0]))))
        .expect("matching packet");
    let mut left = [0.0; 3];
    let mut right = [0.0; 3];
    let mut budget = 4;
    let result = resource.read(None, &mut [&mut left, &mut right], 0..3, &mut budget);
    assert!(matches!(result, ReadOutcome::Full { frames: 3 }));
    assert_eq!(left, [1.0, 2.0, 3.0]);
    assert_eq!(right, left);
}

#[kithara::test(native, tokio)]
async fn epoch_validator_rejects_stale_chunks_after_seek() {
    let mut fixture = PcmFixture::new(4, false).await;
    let receiver = fixture.receiver.take().expect("packet receiver");
    let mut resource = PlayerResource::new(PcmConsumer::new(receiver), Arc::from("test"), &pools())
        .expect("player resource");
    let old = SegmentId::FIRST;
    let current = old.next();
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(old, &[3.0]))))
        .expect("stale packet");
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(old, &[1.0]))))
        .expect("first packet");
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(current, &[2.0]))))
        .expect("current packet");
    resource.select_segment(current);
    let mut left = [-1.0; 1];
    let mut right = [-1.0; 1];
    let mut budget = 4;
    resource.read(None, &mut [&mut left, &mut right], 0..1, &mut budget);
    assert!(!left.contains(&1.0));
    assert!(!left.contains(&3.0));
    assert!(left.contains(&2.0));
    assert_eq!(right, left);
    assert!(fixture.returned().is_some());
    assert!(fixture.returned().is_some());
    assert!(fixture.returned().is_none());
}

#[cfg(feature = "usdt")]
fn consumed_geometry_words(
    events: &[kithara_test_utils::test::usdt::ProbeEvent],
    output_start: u64,
) -> Result<[u64; 24], &'static str> {
    let start = events
        .iter()
        .position(|event| {
            event.probe == "pcm_consumed" && event.field("output_start") == Some(output_start)
        })
        .ok_or("actual consumption has no output interval")?;
    let thread = events[start].thread;
    let mut words = [0; 24];
    let mut next = 0;
    for event in &events[start + 1..] {
        if event.thread != thread {
            continue;
        }
        if event.probe == "pcm_consumed" {
            break;
        }
        if event.probe != "pcm_consumed_geometry" {
            continue;
        }
        if event.field("part") != u64::try_from(next).ok() || next >= 6 {
            return Err("missing or duplicate consumed geometry fragment");
        }
        for (offset, field) in ["first", "second", "third", "fourth"]
            .into_iter()
            .enumerate()
        {
            words[next * 4 + offset] = event.field(field).ok_or("missing geometry word")?;
        }
        next += 1;
    }
    if next != 6 {
        return Err("actual consumption lost exact native geometry");
    }
    Ok(words)
}

#[cfg(feature = "usdt")]
fn fractional_packet(spec: AudioSpec, lane_frame: u64, samples: &[f32]) -> PcmPacket {
    let mut chunk = crate::worker::packet_tests::chunk(
        spec,
        SegmentId::FIRST,
        lane_frame,
        1_000 + lane_frame,
        samples,
    );
    chunk.meta.render_revision = 7;
    chunk.meta.source_span = Some(
        SourceSpan::try_from((
            10_005 + u128::from(lane_frame) * 9,
            9,
            std::num::NonZeroU128::new(10).expect("phase"),
            spec.sample_rate,
            u64::try_from(samples.len()).expect("frames"),
        ))
        .expect("exact fractional geometry")
        .with_render_revision(7)
        .with_mapping_revision(std::num::NonZeroU64::new(11)),
    );
    PcmPacket::Chunk(Box::new(chunk))
}

#[cfg(feature = "usdt")]
fn read_packet_sequence(delayed: bool, base: i64) -> (Vec<f32>, Vec<AudioChunkInfo>) {
    use kithara_signal::{OutputContext, SessionEpoch, SessionFrame};
    use kithara_warp::RenderContext;

    use crate::worker::packet_tests::PacketRing;

    let spec = AudioSpec::new(1, NonZeroU32::new(48_000).expect("rate"));
    let mut ring = PacketRing::new(spec, Duration::from_secs(1), 3);
    ring.push(fractional_packet(spec, 0, &[0.25]));
    if !delayed {
        ring.push(fractional_packet(spec, 1, &[-0.5]));
        ring.push(fractional_packet(spec, 2, &[0.75; 5]));
    }
    let mut resource = PlayerResource::new(
        PcmConsumer::new(ring.receiver.take().expect("receiver")),
        Arc::from("one-one-long"),
        &pools(),
    )
    .expect("resource");
    let mut budget = 8;
    resource.refresh_mark(&mut budget);
    let mut snapshots = vec![resource.source_info().expect("initial packet")];
    let mut pcm = Vec::new();
    for (index, (offset, frames)) in [(0, 1), (1, 1), (2, 5)].into_iter().enumerate() {
        if delayed && index == 1 {
            ring.push(fractional_packet(spec, 1, &[-0.5]));
            ring.push(fractional_packet(spec, 2, &[0.75; 5]));
        }
        let at = base + offset;
        let context = RenderContext::new(
            OutputContext::new(
                SessionFrame::new(at)
                    ..SessionFrame::new(at + i64::try_from(frames).expect("frames")),
                spec.sample_rate,
                SessionEpoch::new(1),
                None,
            )
            .expect("output context"),
            None,
        )
        .expect("render context");
        let mut left = vec![0.0; frames];
        let mut right = vec![0.0; frames];
        assert_eq!(
            resource.read(
                Some(&context),
                &mut [&mut left, &mut right],
                0..frames,
                &mut budget
            ),
            ReadOutcome::Full { frames },
        );
        assert_eq!(left, right);
        pcm.extend(left);
        resource.refresh_mark(&mut budget);
        snapshots.push(resource.source_info().expect("latest snapshot"));
    }
    (pcm, snapshots)
}

#[cfg(feature = "usdt")]
#[kithara::test(native)]
fn consumed_geometry_keeps_a_one_frame_packet_between_maintenance_passes() {
    use kithara_test_utils::test::usdt;

    let trace = usdt::scope();
    for (order, delayed) in [true, false].into_iter().enumerate() {
        let base = 100 + i64::try_from(order).expect("order") * 100;
        let (pcm, snapshots) = read_packet_sequence(delayed, base);
        assert_eq!(pcm, [0.25, -0.5, 0.75, 0.75, 0.75, 0.75, 0.75]);
        let events = trace.events();
        let base = u64::try_from(base).expect("positive base");
        let consumed: Vec<_> = events
            .iter()
            .filter(|event| {
                event.probe == "pcm_consumed"
                    && event
                        .field("output_start")
                        .is_some_and(|at| (base..base + 7).contains(&at))
            })
            .collect();
        assert_eq!(consumed.len(), 3, "three actual packet reads");
        assert_eq!(consumed[1].field("output_start"), Some(base + 1));
        assert_eq!(consumed[1].field("output_end"), Some(base + 2));
        assert_eq!(
            snapshots.iter().any(|info| info.lane_frame == 1),
            !delayed,
            "delayed middle packet must escape latest snapshots"
        );
        let middle = events
            .iter()
            .position(|event| {
                event.probe == "pcm_consumed" && event.field("output_start") == Some(base + 1)
            })
            .expect("actual middle read descriptor");
        let fragment = middle
            + 1
            + events[middle + 1..]
                .iter()
                .position(|event| event.probe == "pcm_consumed_geometry")
                .expect("middle native metadata");
        let mut truncated = events.clone();
        truncated.remove(fragment);
        assert!(
            consumed_geometry_words(&truncated, base + 1).is_err(),
            "missing real metadata must be refused"
        );
        for index in [1, 0, 2] {
            let offset = [0, 1, 2][index];
            let frames = [1, 1, 5][index];
            let numerator = [10_005, 10_014, 10_023][index];
            let words =
                consumed_geometry_words(&events, base + offset).expect("native geometry witness");
            assert_eq!(
                &words[..14],
                &[1, 0, 48_000, frames, 7, 11, numerator, 0, 10, 0, 9, 0, 0, 0]
            );
            assert!(words[14..].iter().all(|word| *word == 0));
        }
    }
    drop(trace);
}
