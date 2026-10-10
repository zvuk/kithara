#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioSpec, OutputContext, SegmentId, SessionEpoch, SessionFrame};
use kithara_test_fixtures::integration_fixtures::constant_half;
use kithara_test_utils::kithara;
use kithara_warp::RenderContext;
use ringbuf::{
    HeapProd, HeapRb,
    traits::{Consumer, Split},
};

use super::*;
use crate::{
    CrossfadeSettings,
    bridge::{DeckEvent, Fade, FadeDir, RtMetrics, Slot, SlotState},
    test_pools::pools,
    worker::{
        PcmPacket,
        packet_tests::{PacketRing, chunk},
    },
};

const ITEM: Slot = Slot::new(1);

#[derive(Clone, Copy)]
enum TrackStateScenario {
    FadeIn,
    FadeOutAfterPlay,
    Play,
    StartPreloading,
    StopAfterPlay,
}
#[derive(Clone, Copy)]
enum ReadOutcomeScenario {
    Playing,
    Finished,
}

struct Fixture {
    track: PlayerTrack,
    packets: PacketRing,
}
impl std::ops::Deref for Fixture {
    type Target = PlayerTrack;
    fn deref(&self) -> &Self::Target {
        &self.track
    }
}
impl std::ops::DerefMut for Fixture {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.track
    }
}
fn spec() -> AudioSpec {
    AudioSpec::new(2, NonZeroU32::new(44_100).expect("sample rate"))
}
fn packet(input: &'static [u8], frames: usize, source: u64, segment: SegmentId) -> PcmPacket {
    let samples: Vec<_> = input
        .chunks_exact(4)
        .cycle()
        .take(frames * 2)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("sample bytes")))
        .collect();
    let pcm = chunk(spec(), segment, 0, source, &samples);
    PcmPacket::Chunk(Box::new(pcm))
}
fn make_track_with(input: &'static [u8], seconds: f64, _slot: Slot) -> Fixture {
    let duration = Duration::from_secs_f64(seconds);
    let frames = u64::try_from(spec().frames_for(duration).expect("source frames").get())
        .expect("source clock");
    let mut packets = PacketRing::new(spec(), duration, 4);
    packets.push(packet(
        input,
        usize::try_from(frames).expect("frames"),
        0,
        SegmentId::FIRST,
    ));
    let mut terminal = chunk(spec(), SegmentId::FIRST, frames, frames, &[]);
    terminal.meta.end_of_track = true;
    packets.push(PcmPacket::Chunk(Box::new(terminal)));
    let resource = PlayerResource::new(
        PcmConsumer::new(packets.receiver.take().expect("receiver")),
        Arc::from("test.mp3"),
        &pools(),
    )
    .expect("resource");
    Fixture {
        track: PlayerTrack::builder()
            .sample_rate(spec().sample_rate)
            .build(Box::new(resource)),
        packets,
    }
}
fn make_track(input: &'static [u8]) -> Fixture {
    make_track_with(input, 60.0, ITEM)
}
fn start(track: &mut PlayerTrack) {
    track.start(Fade::Crossfade(CrossfadeSettings {
        duration: 0.0,
        ..Default::default()
    }));
}
fn render_track(
    track: &mut PlayerTrack,
    scratch: &mut [&mut [f32]],
    bus: &mut [&mut [f32]],
    range: std::ops::Range<usize>,
    sink: &mut RtSink<'_>,
) -> TrackReadOutcome {
    let output = OutputContext::new(
        SessionFrame::new(0)..SessionFrame::new(512),
        spec().sample_rate,
        SessionEpoch::new(0),
        None,
    )
    .expect("output block");
    let context = RenderContext::new_linear(output, None).expect("render context");
    let outcome = track.render(Some(&context), scratch, bus, range.clone(), &mut 8, sink);
    if matches!(
        outcome,
        TrackReadOutcome::Eof | TrackReadOutcome::Partial { .. }
    ) {
        track.poll_end(
            range.start
                + match outcome {
                    TrackReadOutcome::Partial { frames, .. } => frames,
                    _ => 0,
                },
            &mut 8,
            sink,
        );
    }
    outcome
}
fn collect_notifications(rx: &mut impl Consumer<Item = DeckEvent>) -> Vec<DeckEvent> {
    let mut events = Vec::new();
    while let Some(event) = rx.try_pop() {
        events.push(event);
    }
    events
}
fn drain_eof_stop_notifications(
    rx: &mut impl Consumer<Item = DeckEvent>,
    saw_partial: bool,
) -> usize {
    let mut count = 0;
    while let Some(event) = rx.try_pop() {
        if let DeckEvent::Ended { slot, .. } = event {
            assert!(saw_partial, "EOF stop must not precede Partial");
            assert_eq!(slot, ITEM);
            count += 1;
        }
    }
    count
}
#[kithara::test(tokio)]
#[case(TrackStateScenario::StartPreloading, (SlotState::Stopped, true, false))]
#[case(TrackStateScenario::FadeIn, (SlotState::Playing, false, false))]
#[case(TrackStateScenario::FadeOutAfterPlay, (SlotState::Playing, false, true))]
#[case(TrackStateScenario::Play, (SlotState::Playing, true, false))]
#[case(TrackStateScenario::StopAfterPlay, (SlotState::Stopped, true, false))]
async fn track_state_transitions(
    constant_half: &'static [u8],
    #[case] scenario: TrackStateScenario,
    #[case] expected_state: (SlotState, bool, bool),
) {
    let mut track = make_track(constant_half);
    match scenario {
        TrackStateScenario::StartPreloading => {}
        TrackStateScenario::FadeIn => track.start(Fade::Crossfade(CrossfadeSettings::default())),
        TrackStateScenario::FadeOutAfterPlay => {
            start(&mut track);
            track.fade(CrossfadeSettings::default(), FadeDir::Out);
        }
        TrackStateScenario::Play => start(&mut track),
        TrackStateScenario::StopAfterPlay => {
            start(&mut track);
            track.stop(
                Fade::Crossfade(CrossfadeSettings {
                    duration: 0.0,
                    ..Default::default()
                }),
                SessionFrame::new(0),
            );
        }
    }
    assert_eq!(
        (
            track.state(),
            track.fade.settled(),
            track.fade.is_fading_out()
        ),
        expected_state
    );
}

#[kithara::test(tokio)]
async fn track_src_returns_identifier(constant_half: &'static [u8]) {
    let track = make_track(constant_half);
    assert_eq!(&**track.src(), "test.mp3");
}

#[kithara::test(tokio)]
async fn track_initial_position_and_duration(constant_half: &'static [u8]) {
    let track = make_track(constant_half);
    assert_eq!(track.position(), 0.0);
    assert!((track.duration() - 60.0).abs() < f64::EPSILON);
}

#[kithara::test(tokio)]
async fn track_seek_position_is_derived_from_the_media_clock(constant_half: &'static [u8]) {
    let mut track = make_track(constant_half);
    let seconds = 9.791_337;
    let source = u64::try_from(
        spec()
            .frames_for(Duration::from_secs_f64(seconds))
            .expect("seek source frame")
            .get(),
    )
    .expect("source clock");
    let segment = SegmentId::FIRST.next();
    track
        .packets
        .push(packet(constant_half, 512, source, segment));
    track.adopt(segment);
    track.recycle_obsolete(&mut 8);

    let sample_rate = 44_100.0;
    let expected = (seconds * sample_rate).floor() / sample_rate;

    assert!((track.position() - expected).abs() < f64::EPSILON);
}

#[kithara::test(tokio)]
async fn eof_playback_stopped_notification_carries_item_id(constant_half: &'static [u8]) {
    let mut track = make_track_with(constant_half, 0.01, ITEM);
    let (tx, mut rx) = HeapRb::<DeckEvent>::new(8).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    start(&mut track);

    let mut saw_eof_stop = false;
    for _ in 0..4 {
        let _ = render_track(
            &mut track,
            &mut scratch_bufs,
            &mut mix_bufs,
            0..512,
            &mut RtSink::new(
                &mut notification_tx,
                &RtMetrics::default(),
                ITEM,
                SessionFrame::new(0),
            ),
        );

        while let Some(notification) = rx.try_pop() {
            if let DeckEvent::Ended { slot: item_id, .. } = notification {
                saw_eof_stop = item_id == ITEM;
            }
        }

        if saw_eof_stop {
            break;
        }
    }

    assert!(saw_eof_stop);
}

/// Count the EOF stops a run of blocks published, with no ordering claim of
/// its own — the caller's assertion is about whether the end was reported at
/// all, not about what preceded it.
fn count_eof_stops(rx: &mut impl Consumer<Item = DeckEvent>) -> usize {
    collect_notifications(rx)
        .iter()
        .filter(|notification| matches!(notification, DeckEvent::Ended { .. }))
        .count()
}

/// Read past the end of a short track, so the feeder drains and the natural-end
/// path runs on every block.
fn read_past_the_end(track: &mut PlayerTrack, tx: &mut HeapProd<DeckEvent>) {
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    for _ in 0..4 {
        let _ = render_track(
            track,
            &mut scratch_bufs,
            &mut mix_bufs,
            0..512,
            &mut RtSink::new(tx, &RtMetrics::default(), ITEM, SessionFrame::new(0)),
        );
    }
}

/// Bug #5. `seek_seconds` publishes the next epoch *before* it sends the
/// matching `PlayerCmd::Seek`, so a block that renders past EOF in that window
/// renders a position the user has already left. Reporting the end there hands
/// the queue an `ItemDidPlayToEnd` and it auto-advances out from under the seek
/// the processor is about to apply — the track flips while the seek is settling.
#[kithara::test(tokio)]
async fn a_published_seek_holds_the_natural_end_report(constant_half: &'static [u8]) {
    let mut track = make_track_with(constant_half, 0.01, ITEM);
    let (mut notification_tx, mut rx) = HeapRb::<DeckEvent>::new(16).split();

    start(&mut track);
    track.adopt(SegmentId::FIRST.next());
    read_past_the_end(&mut track, &mut notification_tx);

    assert_eq!(
        count_eof_stops(&mut rx),
        0,
        "a track the user has seeked away from must not report its old end"
    );
}

/// The hold is released by the seek itself, so it costs blocks of silence, not
/// the end-of-track signal: once the track is re-based onto the published
/// epoch, the very same drained feeder reports the end.
#[kithara::test(tokio)]
async fn observing_the_seek_epoch_releases_the_held_end(constant_half: &'static [u8]) {
    let mut track = make_track_with(constant_half, 0.01, ITEM);
    let (mut notification_tx, mut rx) = HeapRb::<DeckEvent>::new(16).split();

    start(&mut track);
    track.adopt(SegmentId::FIRST.next());
    read_past_the_end(&mut track, &mut notification_tx);
    let _held = collect_notifications(&mut rx);

    let segment = SegmentId::FIRST.next();
    track.packets.push(packet(constant_half, 441, 0, segment));
    let mut end = chunk(spec(), segment, 441, 441, &[]);
    end.meta.end_of_track = true;
    track.packets.push(PcmPacket::Chunk(Box::new(end)));
    read_past_the_end(&mut track, &mut notification_tx);

    assert_eq!(
        count_eof_stops(&mut rx),
        1,
        "the re-based track must report the end it was holding"
    );
}

#[kithara::test(tokio)]
#[case::playing(ReadOutcomeScenario::Playing)]
#[case::finished(ReadOutcomeScenario::Finished)]
async fn read_outcome_matches_track_state(
    constant_half: &'static [u8],
    #[case] scenario: ReadOutcomeScenario,
) {
    let seconds = match scenario {
        ReadOutcomeScenario::Playing => 60.0,
        ReadOutcomeScenario::Finished => 0.01,
    };
    let mut track = make_track_with(constant_half, seconds, ITEM);
    let (tx, _) = HeapRb::<DeckEvent>::new(8).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    match scenario {
        ReadOutcomeScenario::Playing => start(&mut track),
        ReadOutcomeScenario::Finished => {
            start(&mut track);
            read_past_the_end(&mut track, &mut notification_tx);
        }
    }

    let outcome = render_track(
        &mut track,
        &mut scratch_bufs,
        &mut mix_bufs,
        0..512,
        &mut RtSink::new(
            &mut notification_tx,
            &RtMetrics::default(),
            ITEM,
            SessionFrame::new(0),
        ),
    );

    match scenario {
        ReadOutcomeScenario::Playing => assert!(matches!(
            outcome,
            TrackReadOutcome::Full {
                position,
                duration,
                ..
            } if position >= 0.0 && duration > 0.0
        )),
        ReadOutcomeScenario::Finished => assert!(matches!(outcome, TrackReadOutcome::Eof)),
    }
}

#[kithara::test]
fn decoded_frontier_reads_live_resource_not_stale_render_cache() {
    let mut packets = PacketRing::new(spec(), Duration::from_secs(100), 4);
    let resource = PlayerResource::new(
        PcmConsumer::new(packets.receiver.take().expect("receiver")),
        Arc::from("frontier.flac"),
        &pools(),
    )
    .expect("resource");
    let track = PlayerTrack::builder()
        .sample_rate(spec().sample_rate)
        .build(Box::new(resource));

    assert_eq!(track.decoded_frontier(), 0.0);
    let source = u64::try_from(
        spec()
            .frames_for(Duration::from_secs(81))
            .expect("frontier source frame")
            .get(),
    )
    .expect("source clock");
    packets.push(PcmPacket::Chunk(Box::new(chunk(
        spec(),
        SegmentId::FIRST,
        0,
        source,
        &[],
    ))));
    let live = track.decoded_frontier();
    assert!(
        (live - 81.0).abs() < 1e-6,
        "decoded_frontier must read the live resource, got {live}"
    );
}

#[kithara::test(tokio)]
async fn read_outcome_partial_then_eof(constant_half: &'static [u8]) {
    let mut track = make_track_with(constant_half, 0.01, ITEM);
    let (tx, mut rx) = HeapRb::<DeckEvent>::new(16).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    start(&mut track);

    let mut saw_partial = false;
    let mut saw_eof_after_partial = false;
    let mut eof_stop_count = 0;

    for _ in 0..8 {
        let outcome = render_track(
            &mut track,
            &mut scratch_bufs,
            &mut mix_bufs,
            0..512,
            &mut RtSink::new(
                &mut notification_tx,
                &RtMetrics::default(),
                ITEM,
                SessionFrame::new(0),
            ),
        );

        match outcome {
            TrackReadOutcome::Partial { frames, .. } => {
                assert!(!saw_partial, "expected exactly one Partial outcome");
                assert!(frames > 0);
                saw_partial = true;
            }
            TrackReadOutcome::Eof => {
                if saw_partial {
                    saw_eof_after_partial = true;
                    break;
                }
            }
            TrackReadOutcome::Full { .. } => {}
            TrackReadOutcome::Failed(fault) => {
                panic!("unexpected Failed in this scenario: {fault}")
            }
        }

        eof_stop_count += drain_eof_stop_notifications(&mut rx, saw_partial);
    }

    eof_stop_count += drain_eof_stop_notifications(&mut rx, saw_partial);

    assert!(saw_partial, "expected a Partial outcome before EOF");
    assert!(saw_eof_after_partial, "expected EOF after Partial");
    assert_eq!(
        eof_stop_count, 1,
        "expected exactly one EOF stop notification"
    );
}

#[kithara::test(tokio)]
async fn a_buffered_eof_corrects_an_overestimated_duration(constant_half: &'static [u8]) {
    let mut packets = PacketRing::new(spec(), Duration::from_secs(60), 4);
    packets.push(packet(constant_half, 900, 0, SegmentId::FIRST));
    let mut terminal = chunk(spec(), SegmentId::FIRST, 900, 900, &[]);
    terminal.meta.end_of_track = true;
    packets.push(PcmPacket::Chunk(Box::new(terminal)));
    let resource = PlayerResource::new(
        PcmConsumer::new(packets.receiver.take().expect("receiver")),
        Arc::from("misreported.mp3"),
        &pools(),
    )
    .expect("resource");
    let mut track = PlayerTrack::builder()
        .sample_rate(spec().sample_rate)
        .build(Box::new(resource));
    let (tx, mut rx) = HeapRb::<DeckEvent>::new(16).split();
    let mut notification_tx = tx;
    let mut scratch_l = [0.0; 512];
    let mut scratch_r = [0.0; 512];
    let mut mix_l = [0.0; 512];
    let mut mix_r = [0.0; 512];
    let mut scratch_bufs = [&mut scratch_l[..], &mut scratch_r[..]];
    let mut mix_bufs = [&mut mix_l[..], &mut mix_r[..]];

    start(&mut track);

    let outcome = render_track(
        &mut track,
        &mut scratch_bufs,
        &mut mix_bufs,
        0..512,
        &mut RtSink::new(
            &mut notification_tx,
            &RtMetrics::default(),
            ITEM,
            SessionFrame::new(0),
        ),
    );

    assert!(matches!(
        outcome,
        TrackReadOutcome::Full {
            frames_until_eof: Some(388),
            duration,
            ..
        } if duration < 10.0
    ));
    let notifications = collect_notifications(&mut rx);
    assert!(
        !notifications
            .iter()
            .any(|notification| { matches!(notification, DeckEvent::Ended { .. }) }),
        "the first full block must not emit EOF"
    );
}
