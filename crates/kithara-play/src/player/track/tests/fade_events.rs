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

#[kithara::test(tokio)]
async fn a_fade_out_takes_back_a_play_still_waiting_for_its_track() {
    let mut rig = rig();
    let (mut track, _lane) = loaded(&mut rig, A, "deferred fade").await;
    track.ready = None;
    command_at_zero(&mut track, &mut rig, TrackCommand::Play { at: When::Next });
    assert_eq!(fade_out(&mut track, &mut rig, frame(0)), None);
    track.ready = Some(track.segment);
    rig.with_outbox(|out| track.tick(frame(0), out))
        .expect("live scope");
    assert!(
        rig.block(frame(0), 0.0).is_empty(),
        "a deferred Play leaves no Start behind the fade"
    );
    assert_eq!(track.snapshot().status, TrackStatus::Loaded);
}

#[kithara::test(tokio)]
async fn a_fade_out_before_a_scheduled_start_takes_it_back() {
    let mut rig = rig();
    let (mut track, _lane) = loaded(&mut rig, A, "early fade").await;
    let start = command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Play {
            at: When::At(frame(256)),
        },
    )
    .expect("scheduled Start");
    let fade = fade_out(&mut track, &mut rig, frame(0)).expect("native envelope");
    let mut answers = rig.block(frame(0), 0.0);
    answers.extend(rig.block(frame(256), 0.0));
    assert!(
        answers
            .iter()
            .any(|answer| answer.seq == fade && matches!(answer.outcome, Outcome::Applied { .. })),
    );
    assert!(
        answers.iter().any(|answer| answer.seq == start
            && matches!(answer.outcome, Outcome::Rejected(Rejection::Stale))),
        "the earlier envelope outdates the Start"
    );
    settle(&mut track, &mut rig, &mut answers);
    assert_eq!(track.snapshot().status, TrackStatus::Loaded);
}

#[kithara::test(tokio)]
async fn a_fade_out_after_a_scheduled_start_fades_what_it_started() {
    let mut rig = rig();
    let (mut track, _lane) = loaded(&mut rig, A, "late fade").await;
    let start = command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Play {
            at: When::At(frame(0)),
        },
    )
    .expect("scheduled Start");
    let fade = fade_out(&mut track, &mut rig, frame(128)).expect("native envelope");
    let mut answers = rig.block(frame(0), 0.0);
    answers.extend(rig.block(frame(128), 0.0));
    assert!(
        answers
            .iter()
            .any(|answer| answer.seq == start && matches!(answer.outcome, Outcome::Applied { .. }))
    );
    assert!(answers.iter().any(|answer| answer.seq == fade
        && matches!(answer.outcome, Outcome::Applied { at, .. } if at == frame(128))
        && matches!(answer.batch.commands.as_slice(),
            [DeckPart::Fade { slot, dir: FadeDir::Out, .. }] if *slot == A)));
    settle(&mut track, &mut rig, &mut answers);
    assert_eq!(
        track.snapshot().status,
        TrackStatus::Playing { since: frame(0) }
    );
}
