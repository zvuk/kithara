#![cfg(not(target_arch = "wasm32"))]
use kithara_audio::{DecodeErrorKind, FailureSource, TrackFailureKind};
use kithara_platform::sync::Arc;
use kithara_signal::{SegmentId, SessionFrame};
use kithara_test_fixtures::mock_fixtures::ring_pcm;
use kithara_test_utils::kithara;

use super::super::{PcmConsumer, PlayerResource, ReadOutcome};
use crate::{
    mock::pcm_fixture::{PcmFixture, chunk},
    test_pools::pools,
    worker::PcmPacket,
};
struct RingFixture {
    resource: PlayerResource,
    transport: PcmFixture,
}
impl RingFixture {
    async fn new() -> Self {
        let mut transport = PcmFixture::new(8, false).await;
        let receiver = transport.receiver.take().expect("receiver");
        Self {
            resource: PlayerResource::new(PcmConsumer::new(receiver), Arc::from("moved"), &pools())
                .expect("resource"),
            transport,
        }
    }
    fn push(&mut self, segment: SegmentId, samples: &[f32]) {
        self.transport
            .push(PcmPacket::Chunk(Box::new(chunk(segment, samples))))
            .expect("PCM ring");
    }
    fn eof(&mut self, segment: SegmentId) {
        let mut terminal = chunk(segment, &[]);
        terminal.meta.end_of_track = true;
        self.transport
            .push(PcmPacket::Chunk(Box::new(terminal)))
            .expect("EOF ring");
    }
    fn failure(&mut self, segment: SegmentId) {
        self.transport
            .push(PcmPacket::Failed {
                segment,
                failure: TrackFailureKind::Decode {
                    kind: DecodeErrorKind::InvalidData,
                },
            })
            .expect("failure ring");
    }
    fn read(&mut self) -> (ReadOutcome, [f32; 2]) {
        let mut left = [-1.0; 2];
        let mut right = [-1.0; 2];
        let outcome = self
            .resource
            .read(&mut [&mut left, &mut right], 0..2, &mut 16);
        assert_eq!(right, left);
        (outcome, left)
    }
}

#[kithara::test(native, tokio)]
async fn seek_drain_reports_whether_it_popped_any_item() {
    let mut fixture = RingFixture::new().await;
    fixture.push(SegmentId::FIRST, &[0.1, 0.2]);
    fixture.resource.select_segment(SegmentId::FIRST.next());
    let mut budget = 8;
    fixture.resource.recycle_obsolete(&mut budget);
    assert!(fixture.transport.returned().is_some());
    assert_eq!(budget, 7);
    fixture.resource.recycle_obsolete(&mut budget);
    assert!(fixture.transport.returned().is_none());
    assert_eq!(budget, 7);
}

#[kithara::test(native, tokio)]
async fn a_prime_on_an_empty_ring_returns_instead_of_parking() {
    let mut fixture = RingFixture::new().await;
    assert!(matches!(fixture.read().0, ReadOutcome::Full { frames: 0 }));
}

#[kithara::test(native, tokio)]
async fn a_prime_takes_a_delivered_chunk(ring_pcm: Vec<f32>) {
    let mut fixture = RingFixture::new().await;
    fixture.push(SegmentId::FIRST, &ring_pcm[..2]);
    let (outcome, samples) = fixture.read();
    assert!(matches!(outcome, ReadOutcome::Full { frames: 2 }));
    assert_eq!(samples, ring_pcm[..2]);
}

#[kithara::test(native, tokio)]
async fn consumer_phase_starts_buffering() {
    let mut fixture = RingFixture::new().await;
    assert!(fixture.resource.mark(SessionFrame::new(0)).is_none());
    assert_eq!(fixture.resource.poll_end(&mut 8), None);
}

#[kithara::test(native, tokio)]
async fn consumer_phase_transitions_to_playing_on_first_chunk(ring_pcm: Vec<f32>) {
    let mut fixture = RingFixture::new().await;
    assert!(fixture.resource.mark(SessionFrame::new(0)).is_none());
    fixture.push(SegmentId::FIRST, &ring_pcm[..2]);
    assert!(matches!(fixture.read().0, ReadOutcome::Full { frames: 2 }));
    assert!(fixture.resource.mark(SessionFrame::new(2)).is_some());
}

#[kithara::test(native, tokio)]
async fn consumer_phase_transitions_to_seek_pending() {
    let mut fixture = RingFixture::new().await;
    fixture.resource.select_segment(SegmentId::FIRST.next());
    assert_eq!(fixture.resource.segment(), SegmentId::FIRST.next());
    assert!(fixture.resource.mark(SessionFrame::new(0)).is_none());
}

#[kithara::test(native, tokio)]
async fn consumer_phase_seek_pending_to_playing_on_chunk(ring_pcm: Vec<f32>) {
    let mut fixture = RingFixture::new().await;
    let current = SegmentId::FIRST.next();
    fixture.resource.select_segment(current);
    assert!(fixture.resource.mark(SessionFrame::new(0)).is_none());
    fixture.push(current, &ring_pcm[..2]);
    assert!(matches!(fixture.read().0, ReadOutcome::Full { frames: 2 }));
    assert!(fixture.resource.mark(SessionFrame::new(2)).is_some());
}

#[kithara::test(native, tokio)]
async fn seek_drain_preserves_new_epoch_chunk_after_stale_chunks(ring_pcm: Vec<f32>) {
    let mut fixture = RingFixture::new().await;
    fixture.push(SegmentId::FIRST, &ring_pcm[..2]);
    let current = SegmentId::FIRST.next();
    fixture.push(current, &[0.7, 0.8]);
    fixture.resource.select_segment(current);
    let (outcome, samples) = fixture.read();
    assert!(matches!(outcome, ReadOutcome::Full { frames: 2 }));
    assert_eq!(samples, [0.7, 0.8]);
}

#[kithara::test(native, tokio)]
async fn seek_drain_preserves_new_epoch_eof_after_stale_chunks(ring_pcm: Vec<f32>) {
    let mut fixture = RingFixture::new().await;
    fixture.push(SegmentId::FIRST, &ring_pcm[..2]);
    let current = SegmentId::FIRST.next();
    fixture.eof(current);
    fixture.resource.select_segment(current);
    assert_eq!(fixture.read().0, ReadOutcome::Eof);
    assert_eq!(fixture.resource.poll_end(&mut 8), Some(ReadOutcome::Eof));
}

#[kithara::test(native, tokio)]
async fn consumer_phase_eof_terminates() {
    let mut fixture = RingFixture::new().await;
    fixture.eof(SegmentId::FIRST);
    assert_eq!(fixture.read().0, ReadOutcome::Eof);
    assert_eq!(fixture.resource.poll_end(&mut 8), Some(ReadOutcome::Eof));
}

#[kithara::test(native, tokio)]
async fn consumer_does_not_park_in_terminal_phase() {
    let mut fixture = RingFixture::new().await;
    fixture.eof(SegmentId::FIRST);
    assert_eq!(fixture.resource.poll_end(&mut 8), Some(ReadOutcome::Eof));
    assert_eq!(fixture.read().0, ReadOutcome::Eof);
}

#[kithara::test(native, tokio)]
async fn process_fetch_must_distinguish_failure_from_natural_eof() {
    let mut eof = RingFixture::new().await;
    eof.eof(SegmentId::FIRST);
    assert_eq!(eof.read().0, ReadOutcome::Eof);
    let mut failed = RingFixture::new().await;
    failed.failure(SegmentId::FIRST);
    let outcome = failed.read().0;
    assert_ne!(outcome, ReadOutcome::Eof);
    assert_eq!(
        outcome,
        ReadOutcome::Failed(FailureSource::Producer {
            failure: TrackFailureKind::Decode {
                kind: DecodeErrorKind::InvalidData
            }
        })
    );
}

#[kithara::test(native, tokio)]
async fn a_stale_producer_failure_survives_a_new_seek_epoch() {
    let mut fixture = RingFixture::new().await;
    fixture.failure(SegmentId::FIRST);
    fixture.resource.select_segment(SegmentId::FIRST.next());
    assert_eq!(
        fixture.resource.poll_end(&mut 8),
        Some(ReadOutcome::Failed(FailureSource::ProducerAfterSeek {
            failure: TrackFailureKind::Decode {
                kind: DecodeErrorKind::InvalidData
            }
        }))
    );
}

#[kithara::test(native, tokio)]
async fn a_stale_natural_eof_does_not_terminate_a_new_seek_epoch() {
    let mut fixture = RingFixture::new().await;
    fixture.eof(SegmentId::FIRST);
    fixture.resource.select_segment(SegmentId::FIRST.next());
    assert_eq!(fixture.resource.poll_end(&mut 8), None);
}

#[kithara::test(native, tokio)]
async fn a_stale_producer_failure_terminates_the_consumer() {
    let mut fixture = RingFixture::new().await;
    fixture.failure(SegmentId::FIRST);
    fixture
        .resource
        .select_segment(SegmentId::FIRST.next().next().next());
    assert_eq!(
        fixture.resource.poll_end(&mut 8),
        Some(ReadOutcome::Failed(FailureSource::ProducerAfterSeek {
            failure: TrackFailureKind::Decode {
                kind: DecodeErrorKind::InvalidData
            }
        }))
    );
}

#[kithara::test(native, tokio)]
async fn consumer_phase_terminal() {
    let mut fixture = RingFixture::new().await;
    assert!(!matches!(
        fixture.read().0,
        ReadOutcome::Eof | ReadOutcome::Failed(_)
    ));
    fixture.push(SegmentId::FIRST, &[0.1, 0.2]);
    assert!(!matches!(
        fixture.read().0,
        ReadOutcome::Eof | ReadOutcome::Failed(_)
    ));
    fixture.resource.select_segment(SegmentId::FIRST.next());
    assert!(!matches!(
        fixture.read().0,
        ReadOutcome::Eof | ReadOutcome::Failed(_)
    ));
    fixture.eof(SegmentId::FIRST.next());
    assert!(matches!(fixture.read().0, ReadOutcome::Eof));
    let mut fixture = RingFixture::new().await;
    fixture.failure(SegmentId::FIRST);
    assert!(matches!(fixture.read().0, ReadOutcome::Failed(_)));
}
