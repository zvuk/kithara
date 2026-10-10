use kithara_platform::time::Duration;
use kithara_stream::SourcePhase;
use kithara_test_fixtures::unit_fixtures::{RoutePcm, route_pcm};
use kithara_test_utils::kithara;

use super::rebuild::{route_signal_source, test_source};
use crate::{
    AudioSource, SeekOutcome, TrackStep, WaitingReason, pipeline::source::core::OwnerPhase,
};
#[kithara::test(tokio)]
async fn is_terminal_for_each_phase() {
    let mut fixture = test_source(0).await;
    for phase in [OwnerPhase::Decoding, OwnerPhase::AtEof] {
        fixture.source.phase = phase;
        assert!(
            !matches!(fixture.source.step_track(), TrackStep::Failed(_)),
            "expected recoverable phase"
        );
    }
    fixture.source.phase = OwnerPhase::Failed {
        failure: crate::TrackFailureKind::Decode {
            kind: crate::DecodeErrorKind::Interrupted,
        },
        error: Some(crate::DecodeError::Interrupted),
    };
    assert!(matches!(fixture.source.step_track(), TrackStep::Failed(_)));
}
#[kithara::test(tokio)]
async fn map_source_phase_table(route_pcm: RoutePcm) {
    for (phase, expected) in [
        (SourcePhase::Waiting, Some(WaitingReason::Waiting)),
        (
            SourcePhase::WaitingDemand,
            Some(WaitingReason::WaitingDemand),
        ),
        (
            SourcePhase::WaitingMetadata,
            Some(WaitingReason::WaitingMetadata),
        ),
        (SourcePhase::Ready, None),
        (SourcePhase::Eof, None),
    ] {
        let mut fixture = route_signal_source(&route_pcm, crate::consts::SAMPLE_RATE).await;
        *fixture.phase.lock() = phase;
        let reason = match fixture.source.step_track() {
            TrackStep::Blocked(reason) => Some(reason),
            _ => None,
        };
        assert_eq!(reason, expected);
    }
    let mut fixture = test_source(0).await;
    fixture.source.phase = OwnerPhase::Failed {
        failure: crate::TrackFailureKind::Decode {
            kind: crate::DecodeErrorKind::Interrupted,
        },
        error: Some(crate::DecodeError::Interrupted),
    };
    assert!(matches!(fixture.source.step_track(), TrackStep::Failed(_)));
}
#[kithara::test]
fn seek_context_copy_and_eq() {
    let ctx = SeekOutcome::Landed {
        target: Duration::from_millis(500),
        landed_at: Duration::from_secs(42),
    };
    let copy = ctx;
    assert_eq!(ctx, copy);
    let SeekOutcome::Landed { target, landed_at } = copy else {
        panic!("copied synchronous landing");
    };
    assert_eq!(landed_at, Duration::from_secs(42));
    assert_eq!(target, Duration::from_millis(500));
}
#[kithara::test(tokio)]
async fn at_eof_allows_seek_transition() {
    let mut fixture = test_source(0).await;
    fixture.source.phase = OwnerPhase::AtEof;
    assert!(matches!(fixture.source.step_track(), TrackStep::Eof));
    fixture
        .source
        .seek(Duration::from_secs(5))
        .expect("seek reopens exhausted source");
    assert!(matches!(fixture.source.phase, OwnerPhase::Decoding));
}
