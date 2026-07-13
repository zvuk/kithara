use crossbeam_queue::ArrayQueue;
use kithara_decode::DecodeError;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::MediaInfo;
use kithara_test_utils::kithara;

use super::*;
use crate::pipeline::{
    rebuild::{RebuildState, RecreateCause, RecreateNext, RecreateState},
    seek::{ApplySeekState, ResumeState, SeekContext, SeekMode, SeekRequest},
    track::{TrackFailure, WaitContext, WaitingReason},
};

fn seek_ctx() -> SeekContext {
    SeekContext {
        epoch: 1,
        target: Duration::from_secs(5),
    }
}

fn seek_req() -> SeekRequest {
    SeekRequest {
        seek: seek_ctx(),
        ..Default::default()
    }
}

fn recreate_state() -> RecreateState {
    RecreateState {
        cause: RecreateCause::FormatBoundary,
        media_info: MediaInfo::default(),
        next: RecreateNext::Decode,
        offset: 0,
    }
}

fn rebuild_state() -> RebuildState {
    RebuildState {
        ticket: 1,
        recreate: recreate_state(),
        started_seek_epoch: 0,
        completion: Arc::new(ArrayQueue::new(1)),
        superseded_seek: None,
    }
}

#[kithara::test]
fn playing_for_state_active_states_are_true() {
    assert!(playing_for_state(&Track::<Decoding>::new(()).erase()));
    assert!(playing_for_state(
        &Track::<SeekRequested>::new(seek_req()).erase()
    ));
    assert!(playing_for_state(
        &Track::<ApplyingSeek>::new(ApplySeekState {
            mode: SeekMode::Direct { target_byte: None },
            request: seek_req(),
        })
        .erase()
    ));
    assert!(playing_for_state(
        &Track::<WaitingForSource>::new(WaitState {
            context: WaitContext::Playback,
            reason: WaitingReason::Waiting,
        })
        .erase()
    ));
    assert!(playing_for_state(
        &Track::<RecreatingDecoder>::new(RecreateState {
            cause: RecreateCause::FormatBoundary,
            media_info: MediaInfo::default(),
            next: RecreateNext::Decode,
            offset: 0,
        })
        .erase()
    ));
    assert!(playing_for_state(
        &Track::<RebuildingDecoder>::new(rebuild_state()).erase()
    ));
    assert!(playing_for_state(
        &Track::<AwaitingResume>::new(ResumeState {
            seek: seek_ctx(),
            ..Default::default()
        })
        .erase()
    ));
}

#[kithara::test]
fn playing_for_state_terminal_states_are_false() {
    assert!(!playing_for_state(&Track::<AtEof>::new(()).erase()));
    assert!(!playing_for_state(
        &Track::<Failed>::new(TrackFailure::SourceCancelled).erase()
    ));
    assert!(!playing_for_state(
        &Track::<Failed>::new(TrackFailure::RecreateFailed { offset: 0 }).erase()
    ));
    assert!(!playing_for_state(
        &Track::<Failed>::new(TrackFailure::Decode(DecodeError::Interrupted)).erase()
    ));
}

#[kithara::test]
fn playing_matrix_covers_every_transition_endpoint() {
    let transitions: &[(CurrentFsm, bool)] = &[
        (Track::<Decoding>::new(()).erase(), true),
        (Track::<SeekRequested>::new(seek_req()).erase(), true),
        (
            Track::<ApplyingSeek>::new(ApplySeekState {
                mode: SeekMode::Direct { target_byte: None },
                request: seek_req(),
            })
            .erase(),
            true,
        ),
        (
            Track::<WaitingForSource>::new(WaitState {
                context: WaitContext::Playback,
                reason: WaitingReason::Waiting,
            })
            .erase(),
            true,
        ),
        (
            Track::<RecreatingDecoder>::new(RecreateState {
                cause: RecreateCause::VariantSwitch,
                media_info: MediaInfo::default(),
                next: RecreateNext::Decode,
                offset: 0,
            })
            .erase(),
            true,
        ),
        (
            Track::<RebuildingDecoder>::new(rebuild_state()).erase(),
            true,
        ),
        (
            Track::<AwaitingResume>::new(ResumeState {
                seek: seek_ctx(),
                ..Default::default()
            })
            .erase(),
            true,
        ),
        (Track::<AtEof>::new(()).erase(), false),
        (
            Track::<Failed>::new(TrackFailure::SourceCancelled).erase(),
            false,
        ),
    ];
    for (state, expected) in transitions {
        assert_eq!(playing_for_state(state), *expected);
    }
}

#[kithara::test]
fn no_spurious_flip_across_100_decoding_transitions() {
    for _ in 0..100 {
        assert!(
            playing_for_state(&Track::<Decoding>::new(()).erase()),
            "PLAYING must stay true across a long Decoding → Decoding loop"
        );
    }
}
