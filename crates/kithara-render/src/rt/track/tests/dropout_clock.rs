#![cfg(not(target_arch = "wasm32"))]
use std::num::NonZeroU32;

use kithara_platform::sync::Arc;
use kithara_signal::{OutputContext, SegmentId, SessionEpoch, SessionFrame};
use kithara_test_utils::kithara;
use kithara_warp::RenderContext;
use ringbuf::{
    HeapRb,
    traits::{Consumer, Split},
};

use super::super::{PcmConsumer, PlayerResource, PlayerTrack, RtSink, TrackReadOutcome};
use crate::{
    bridge::{DeckEvent, Fade, RtMetrics, Slot, TrackClock},
    mock::pcm_fixture::{PcmFixture, chunk},
    test_pools::pools,
    worker::PcmPacket,
};

const BLOCK: usize = 2;

struct Rig {
    fixture: PcmFixture,
    track: PlayerTrack,
    context: RenderContext,
    events: Vec<DeckEvent>,
    metrics: RtMetrics,
}

impl Rig {
    async fn new(clock: TrackClock) -> Self {
        let mut fixture = PcmFixture::new(8, false).await;
        let resource = PlayerResource::new(
            PcmConsumer::new(fixture.receiver.take().expect("receiver")),
            Arc::from("dropout-clock"),
            &pools(),
        )
        .expect("resource");
        let rate = NonZeroU32::new(48_000).expect("rate");
        let mut track = PlayerTrack::builder()
            .item_id(kithara_events::TrackId::allocate())
            .sample_rate(rate)
            .build(Box::new(resource));
        track.start(Fade::Crossfade(crate::CrossfadeSettings {
            duration: 0.0,
            ..Default::default()
        }));
        track.set_clock(clock);
        let start = SessionFrame::new(0);
        let output = OutputContext::new(
            start..SessionFrame::new(2),
            rate,
            SessionEpoch::new(1),
            None,
        )
        .expect("output range");
        Self {
            fixture,
            track,
            context: RenderContext::new(output, None).expect("render context"),
            events: Vec::new(),
            metrics: RtMetrics::default(),
        }
    }

    fn push(&mut self, segment: SegmentId, lane_frame: u64, samples: &[f32]) {
        let mut packet = chunk(segment, samples);
        packet.meta.lane_frame = lane_frame;
        self.fixture
            .push(PcmPacket::Chunk(Box::new(packet)))
            .expect("lane room");
    }

    /// Renders one block and returns what the track played into its bus.
    fn block(&mut self) -> (TrackReadOutcome, [f32; BLOCK]) {
        let (mut producer, mut consumer) = HeapRb::<DeckEvent>::new(8).split();
        let mut left = [0.0; BLOCK];
        let mut right = [0.0; BLOCK];
        let mut bus_left = [0.0; BLOCK];
        let mut bus_right = [0.0; BLOCK];
        let mut sink = RtSink::new(
            &mut producer,
            &self.metrics,
            Slot::new(0),
            SessionFrame::new(0),
        );
        let outcome = self.track.render(
            Some(&self.context),
            &mut [&mut left, &mut right],
            &mut [&mut bus_left, &mut bus_right],
            0..BLOCK,
            &mut 16,
            &mut sink,
        );
        while let Some(event) = consumer.try_pop() {
            self.events.push(event);
        }
        (outcome, bus_left)
    }

    fn starve(&mut self, blocks: usize) {
        for _ in 0..blocks {
            assert!(matches!(
                self.block().0,
                TrackReadOutcome::Full { frames: 0, .. }
            ));
        }
    }

    fn underrun_ends(&self) -> Vec<(u32, bool)> {
        self.events
            .iter()
            .filter_map(|event| match *event {
                DeckEvent::UnderrunEnded {
                    frames, recovered, ..
                } => Some((frames, recovered)),
                _ => None,
            })
            .collect()
    }
}

const PCM: [f32; 8] = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8];

#[kithara::test(native, tokio)]
async fn a_session_time_track_drops_the_frames_its_dropout_missed() {
    let mut rig = Rig::new(TrackClock::Session).await;
    rig.starve(2);
    rig.push(SegmentId::FIRST, 0, &PCM);

    let (outcome, played) = rig.block();

    assert!(matches!(outcome, TrackReadOutcome::Full { frames: 2, .. }));
    assert_eq!(
        played,
        [0.5, 0.6],
        "lane frames 4 and 5 sound on session frames 4 and 5"
    );
    assert_eq!(rig.underrun_ends(), [(4, true)]);
}

#[kithara::test(native, tokio)]
async fn a_media_time_track_resumes_on_the_frame_it_stopped_on() {
    let mut rig = Rig::new(TrackClock::Media).await;
    rig.starve(2);
    rig.push(SegmentId::FIRST, 0, &PCM);

    let (outcome, played) = rig.block();

    assert!(matches!(outcome, TrackReadOutcome::Full { frames: 2, .. }));
    assert_eq!(played, [0.1, 0.2]);
    assert_eq!(rig.underrun_ends(), [(4, true)]);
}

#[kithara::test(native, tokio)]
async fn pcm_short_of_the_debt_is_dropped_and_the_rest_stays_owed() {
    let mut rig = Rig::new(TrackClock::Session).await;
    rig.starve(2);
    rig.push(SegmentId::FIRST, 0, &PCM[..3]);

    let (outcome, played) = rig.block();

    assert!(matches!(outcome, TrackReadOutcome::Full { frames: 0, .. }));
    assert_eq!(played, [0.0; BLOCK]);
    assert!(
        rig.underrun_ends().is_empty(),
        "the track is still behind the session"
    );

    rig.push(SegmentId::FIRST, 3, &[1.3, 1.4, 1.5, 1.6, 1.7, 1.8]);
    let (_, played) = rig.block();

    assert_eq!(
        played,
        [1.6, 1.7],
        "six frames missed, three dropped earlier"
    );
    assert_eq!(rig.underrun_ends(), [(6, true)]);
}

#[kithara::test(native, tokio)]
async fn an_adopted_segment_drops_nothing_and_ends_the_dropout_unrecovered() {
    let mut played = Vec::new();
    for clock in [TrackClock::Media, TrackClock::Session] {
        let mut rig = Rig::new(clock).await;
        rig.starve(2);
        let segment = SegmentId::FIRST.next();
        rig.track.adopt(segment);
        rig.push(segment, 0, &PCM);

        played.push(rig.block().1);
        assert!(rig.underrun_ends().is_empty(), "adopt cleared the gap");
    }

    assert_eq!(
        played[1], played[0],
        "the adopted segment sounds from its start under either clock"
    );
}

#[kithara::test(native, tokio)]
async fn leaving_session_time_forgives_the_debt() {
    let mut rig = Rig::new(TrackClock::Session).await;
    rig.starve(2);
    rig.track.set_clock(TrackClock::Media);
    rig.push(SegmentId::FIRST, 0, &PCM);

    let (_, played) = rig.block();

    assert_eq!(played, [0.1, 0.2]);
    assert_eq!(rig.underrun_ends(), [(4, true)]);
}

/// Two frames play on session frames 0 and 1, then the track starves through
/// session frame 5; the mark it publishes on frame 6 maps frame 8 onto the
/// lane frame that will sound there.
#[kithara::test(native, tokio)]
#[case::media_time(TrackClock::Media, 4)]
#[case::session_time(TrackClock::Session, 8)]
async fn the_mark_of_a_starving_track_maps_the_session_by_its_clock(
    #[case] clock: TrackClock,
    #[case] lane: u64,
) {
    let mut rig = Rig::new(clock).await;
    rig.push(SegmentId::FIRST, 0, &PCM[..2]);
    rig.block();
    rig.starve(2);

    let mark = rig
        .track
        .mark(SessionFrame::new(6))
        .expect("a playing track keeps its mark");

    assert_eq!(
        mark.lane_at(SessionFrame::new(8)).map(|at| at.frame),
        Some(lane)
    );
}

/// The clock in force when PCM returns decides the dropout, so a switch to
/// session time one block into it drops the whole dropout, as a track on
/// session time from the start does.
#[kithara::test(native, tokio)]
async fn a_switch_to_session_time_inside_a_dropout_drops_the_whole_dropout() {
    let mut rig = Rig::new(TrackClock::Media).await;
    rig.starve(1);
    rig.track.set_clock(TrackClock::Session);
    rig.starve(1);
    rig.push(SegmentId::FIRST, 0, &PCM);

    let (_, played) = rig.block();

    assert_eq!(played, [0.5, 0.6]);
    assert_eq!(rig.underrun_ends(), [(4, true)]);
}
