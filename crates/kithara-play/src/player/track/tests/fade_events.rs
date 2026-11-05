use super::*;

#[kithara::test]
#[case::eof(false)]
#[case::failed(true)]
async fn a_delayed_faded_event_preserves_the_tracks_terminal_status(#[case] failed: bool) {
    let mut rig = rig();
    let (mut track, _lane) = loaded(&mut rig, A, "terminal fade").await;
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

fn fade_out(track: &mut Track, rig: &mut Rig, at: SessionFrame) -> Option<Seq> {
    rig.with_outbox(|out| {
        track.apply(
            TrackCommand::Fade {
                at: When::At(at),
                settings: CrossfadeSettings::default(),
                dir: FadeDir::Out,
            },
            out,
        )
    })
    .expect("live scope")
    .expect("fade admission")
}

/// A fade-out takes back a Play that has not started, waiting for its track
/// (`waits`) or scheduled after the fade, and fades a Play it follows. The
/// rows give the Play's frame, the fade's, and whether the Start applies.
#[kithara::test(tokio)]
#[case::while_its_play_waits_for_the_track(true, When::Next, 0, None)]
#[case::before_a_scheduled_start(false, When::At(frame(256)), 0, Some(false))]
#[case::after_a_scheduled_start(false, When::At(frame(0)), 128, Some(true))]
async fn a_fade_out_takes_back_a_play_it_precedes_and_fades_one_it_follows(
    #[case] waits: bool,
    #[case] play: When<SessionFrame>,
    #[case] fade_at: i64,
    #[case] started: Option<bool>,
) {
    let mut rig = rig();
    let (mut track, _lane) = loaded(&mut rig, A, "fade").await;
    if waits {
        track.ready = None;
    }
    let start = command_at_zero(&mut track, &mut rig, TrackCommand::Play { at: play });
    let fade = fade_out(&mut track, &mut rig, frame(fade_at));
    if waits {
        track.ready = Some(track.segment);
        rig.with_outbox(|out| track.tick(frame(0), out))
            .expect("live scope");
    }
    let mut answers: Vec<_> = [0, 128, 256]
        .into_iter()
        .flat_map(|at| rig.block(frame(at), 0.0))
        .collect();
    assert_eq!(answers.is_empty(), waits, "a deferred Play leaves no Start");
    let applied = |seq: Option<Seq>| {
        let answer = answers.iter().find(|answer| Some(answer.seq) == seq)?;
        Some(match answer.outcome {
            Outcome::Applied { .. } => true,
            Outcome::Rejected(Rejection::Stale) => false,
            Outcome::Rejected(rejection) => panic!("{rejection:?}"),
        })
    };
    assert_eq!(applied(start), started, "the Start");
    assert_eq!(fade.is_some(), !waits, "a deferred Play takes no envelope");
    if let Some(fade) = fade {
        assert!(answers.iter().any(|answer| answer.seq == fade
            && matches!(answer.outcome, Outcome::Applied { at, .. } if at == frame(fade_at))
            && matches!(answer.batch.commands.as_slice(),
                [DeckPart::Fade { slot, dir: FadeDir::Out, .. }] if *slot == A)));
    }
    settle(&mut track, &mut rig, &mut answers);
    let status = if started == Some(true) {
        TrackStatus::Playing { since: frame(0) }
    } else {
        TrackStatus::Loaded
    };
    assert_eq!(track.snapshot().status, status);
}
