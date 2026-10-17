use super::*;

#[kithara::test]
#[case::eof(false)]
#[case::failed(true)]
fn a_delayed_faded_event_preserves_the_tracks_terminal_status(#[case] failed: bool) {
    let mut rig = rig();
    let (mut track, _lane) = loaded(&mut rig, A, "terminal fade");
    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Play {
            at: When::At(frame(0)),
        },
    );
    let mut answers = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut answers);
    assert_eq!(
        track.snapshot().status,
        TrackStatus::Playing { since: frame(0) }
    );
    let terminal = frame(2_048);
    let fault = PlaybackFault::Source(kithara_audio::TrackFailureKind::SourceCancelled);
    let event = if failed {
        DeckEvent::Failed {
            slot: A,
            at: terminal,
            fault,
        }
    } else {
        DeckEvent::Ended {
            slot: A,
            at: terminal,
        }
    };
    rig.with_outbox(|out| track.settle(TrackReceipt::Event(event), out))
        .expect("live scope");
    let status = if failed {
        TrackStatus::Failed {
            at: terminal,
            fault,
        }
    } else {
        TrackStatus::Ended { at: terminal }
    };
    assert_eq!(
        track.snapshot().status,
        status,
        "terminal event applied first"
    );
    rig.with_outbox(|out| {
        track.settle(
            TrackReceipt::Event(DeckEvent::Faded {
                slot: A,
                at: frame(4_096),
            }),
            out,
        )
    })
    .expect("live scope");
    assert_eq!(
        track.snapshot().status,
        status,
        "a later envelope event keeps the EOF/failure"
    );
}
