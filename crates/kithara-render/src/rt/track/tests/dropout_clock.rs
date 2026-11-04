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
    let mut rig = Rig::new(TrackClock::Session).await;
    rig.starve(2);
    let segment = SegmentId::FIRST.next();
    rig.track.adopt(segment);
    rig.push(segment, 0, &PCM);

    let (_, played) = rig.block();

    assert_eq!(played, [0.1, 0.2]);
    assert!(rig.underrun_ends().is_empty(), "adopt cleared the gap");
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
