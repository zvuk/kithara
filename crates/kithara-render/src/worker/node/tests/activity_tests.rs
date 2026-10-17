use std::num::NonZeroU32;

use kithara_audio::{DecodeErrorKind, Fetch, TrackFailureKind, TrackStep, WaitingReason};
use kithara_command::{Batch, When};
use kithara_signal::{AudioChunk, AudioSpec, SegmentId};
use kithara_test_utils::kithara;
use kithara_worker::{Task, TickResult};

use crate::mock::{chunk, node_fixture::NodeFixture};

fn request_seek(fixture: &mut NodeFixture) {
    fixture
        .lane
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![crate::LaneCommand::Segment {
                    id: SegmentId::FIRST.next(),
                    from: kithara_platform::time::Duration::from_secs(5),
                    speed: kithara_sync::SpeedCurve::Constant(1.0),
                }],
            },
        )
        .expect("seek command");
}

fn decoded() -> TrackStep<AudioChunk> {
    let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
    TrackStep::Produced(Fetch::data(chunk(
        spec,
        SegmentId::FIRST,
        0,
        0,
        &[0.5; 2048],
    )))
}

fn active_steps() -> [TrackStep<AudioChunk>; 5] {
    [
        decoded(),
        TrackStep::StateChanged,
        TrackStep::Blocked(WaitingReason::Waiting),
        TrackStep::Blocked(WaitingReason::WaitingMetadata),
        TrackStep::Blocked(WaitingReason::WaitingDemand),
    ]
}

#[kithara::test(tokio)]
async fn playing_for_state_active_states_are_true() {
    for step in active_steps() {
        let mut fixture = NodeFixture::with_steps(2, [step]).await;
        fixture.node.tick();
        fixture.node.recycle();
        assert!(fixture.activity.is_playing());
    }
    let mut fixture = NodeFixture::new(2).await;
    request_seek(&mut fixture);
    assert!(fixture.activity.is_playing());
    fixture.node.synchronize().expect("apply source seek");
    assert_eq!(
        fixture.node.source.cursor().segment,
        SegmentId::FIRST.next()
    );
    assert!(fixture.activity.is_playing());
}

#[kithara::test(tokio)]
async fn playing_for_state_terminal_states_are_false() {
    let mut fixture = NodeFixture::with_steps(2, [TrackStep::Eof]).await;
    assert!(fixture.activity.is_playing());
    fixture.node.tick();
    fixture.node.tick();
    fixture.node.recycle();
    assert!(!fixture.activity.is_playing());
    for failure in [
        TrackFailureKind::SourceCancelled,
        TrackFailureKind::RecreateFailed { offset: 0 },
        TrackFailureKind::Decode {
            kind: DecodeErrorKind::Interrupted,
        },
    ] {
        let mut fixture = NodeFixture::with_steps(2, [TrackStep::Failed(failure)]).await;
        assert!(fixture.activity.is_playing());
        fixture.node.tick();
        fixture.node.recycle();
        assert!(!fixture.activity.is_playing());
    }
    let mut fixture = NodeFixture::new(2).await;
    assert!(fixture.activity.is_playing());
    fixture.node.on_cancel();
    assert!(!fixture.activity.is_playing());
}

#[kithara::test(tokio)]
async fn playing_matrix_covers_every_transition_endpoint() {
    for (step, expected) in active_steps().into_iter().map(|step| (step, true)).chain([
        (TrackStep::Produced(Fetch::NaturalEof), false),
        (TrackStep::Failed(TrackFailureKind::SourceCancelled), false),
    ]) {
        let mut fixture = NodeFixture::with_steps(2, [step]).await;
        fixture.node.tick();
        fixture.node.recycle();
        assert_eq!(fixture.activity.is_playing(), expected);
    }
    let mut fixture = NodeFixture::new(2).await;
    request_seek(&mut fixture);
    assert!(fixture.activity.is_playing());
    fixture.node.synchronize().expect("apply source seek");
    assert_eq!(
        fixture.node.source.cursor().segment,
        SegmentId::FIRST.next()
    );
    assert!(fixture.activity.is_playing());
}

#[kithara::test(tokio)]
async fn no_spurious_flip_across_100_decoding_transitions() {
    let steps = (0..100).map(|_| decoded());
    let mut fixture = NodeFixture::with_steps(2, steps).await;
    for _ in 0..100 {
        assert_eq!(fixture.node.tick(), TickResult::Progress);
        assert!(fixture.receiver.pop().is_some(), "decoding produces PCM");
        fixture.node.recycle();
        assert!(
            fixture.activity.is_playing(),
            "PLAYING must stay true across a long Decoding loop"
        );
    }
}

#[kithara::test(tokio)]
async fn a_commanded_seek_reactivates_source_activity() {
    let mut fixture = NodeFixture::with_steps(2, [TrackStep::Eof]).await;
    assert!(fixture.activity.is_playing());
    fixture.node.tick();
    fixture.node.tick();
    assert!(!fixture.activity.is_playing());
    request_seek(&mut fixture);
    fixture.node.synchronize().expect("source seek");
    assert!(
        fixture.activity.is_playing(),
        "seek resumes the active source lifecycle"
    );
}
