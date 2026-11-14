use super::*;
use crate::{Bound, TrackSettingsChange};

fn answer_source(
    track: &mut Track,
    rig: &mut Rig,
    receipt: Receipt<DispatcherProtocol<ResourceLoad<TestPools>>>,
) -> Settled {
    rig.with_outbox(|out| track.settle(TrackReceipt::Loaded(receipt), out))
        .expect("live deck scope")
}

#[kithara::test]
fn lane_retirement_failed_open_finishes_its_pending_configuration() {
    let mut rig = rig();
    let mut track = track(A);
    load(&mut track, &mut rig, Position::ZERO);
    let old = command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Configure(TrackSettingsChange::Keylock(true), When::Next),
    )
    .expect("configuration in opening lane");
    assert!(track.snapshot().pending_lane);
    assert!(track.projected().keylock());
    let receipt = rig.open(Err(LoadRefusal::Cancelled)).expect("refused open");
    answer_source(&mut track, &mut rig, receipt);
    assert_eq!(track.status(), TrackStatus::Idle);
    assert!(
        !track.snapshot().pending_lane,
        "the closed inbox answers its commands"
    );
    assert!(
        !track.projected().keylock(),
        "arrived refusals settle before handover"
    );

    assert!(matches!(
        in_pass(&mut rig, frame(0), |out| track.apply(
            TrackCommand::Load {
                item: item(),
                position: Position::ZERO,
            },
            out
        )),
        Err(PlayError::NotReady)
    ));
    assert!(
        matches!(track.lane_receipt(), Some(Settled::Rejected { seq, reason: Rejection::Unanswered }) if seq == old)
    );
    assert!(track.lane_receipt().is_none());
    load(&mut track, &mut rig, Position::ZERO);
    assert!(!track.snapshot().pending_lane);
    let new = command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Configure(TrackSettingsChange::Keylock(true), When::Next),
    )
    .expect("new scope configuration");
    assert_eq!(new, old, "bare sequence numbers repeat across channels");
    assert!(track.snapshot().pending_lane);
    let receipt = rig
        .open(Err(LoadRefusal::Cancelled))
        .expect("second refused open");
    answer_source(&mut track, &mut rig, receipt);
    assert!(!track.snapshot().pending_lane);
    assert!(!track.projected().keylock());
}

#[kithara::test]
#[case::refused(false)]
#[case::opened(true)]
async fn lane_retirement_during_open_keeps_admission_closed(#[case] opened: bool) {
    let mut rig = rig();
    let mut track = track(A);
    load(&mut track, &mut rig, Position::ZERO);
    command_at_zero(&mut track, &mut rig, TrackCommand::Release);
    assert_eq!(track.status(), TrackStatus::Released);
    assert_eq!(track.snapshot().lane_room, 0);
    assert!(
        command_at_zero(
            &mut track,
            &mut rig,
            TrackCommand::Configure(TrackSettingsChange::Keylock(true), When::Next),
        )
        .is_none(),
        "configuration without an accepting executor stays local"
    );
    let latency = track.snapshot().engine_latency;
    let result = if opened {
        let mut loaded = opened_fixture(&mut rig, "retired open").await;
        loaded.engine_latency = FrameCount::new(123);
        Ok(loaded)
    } else {
        Err(LoadRefusal::Cancelled)
    };
    let receipt = rig.open(result).expect("retired opening");
    answer_source(&mut track, &mut rig, receipt);
    assert_eq!(track.status(), TrackStatus::Released);
    assert_eq!(track.snapshot().lane_room, 0);
    assert!(!track.snapshot().pending_lane);
    assert_eq!(track.snapshot().engine_latency, latency);
    assert!(!track.segment.ready);
    assert!(track.source_basis.is_none() && track.observed_source.is_none());
}

#[derive(Clone, Copy, Debug)]
enum RetirementDrive {
    NewLoad,
    Tick,
    Explicit,
}

#[kithara::test]
#[case::new_load_is_blocked(RetirementDrive::NewLoad)]
#[case::retirement_retries_and_reloads(RetirementDrive::Tick)]
#[case::fresh_source_starts_first(RetirementDrive::Explicit)]
async fn lane_retirement_survives_a_full_dispatcher(#[case] drive: RetirementDrive) {
    let mut rig = rig();
    let mut track = track(A);
    load(&mut track, &mut rig, Position::ZERO);
    let (sender, inbox) = lane();
    replace_lane_sender(&mut track.lane, sender);
    let mut inbox = Some(inbox);
    command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Configure(TrackSettingsChange::Speed(1.25), When::Next),
    );
    {
        let deck = &mut *rig;
        let mut scope = deck.ring.scope(deck.scope).expect("live deck");
        for _ in 0..scope.available() {
            scope
                .send(
                    When::Next,
                    Batch {
                        basis: Vec::new(),
                        commands: Vec::new(),
                    },
                )
                .expect("fill deck credits");
        }
        assert_eq!(scope.available(), 0);
    }
    let opened = opened_fixture(&mut rig, "retiring").await;
    let resident = opened.lane;
    let receipt = rig.open(Ok(opened)).expect("opened source");
    for _ in 0..rig.dispatcher.available() {
        rig.dispatcher
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![DispatcherCommand::SetPriority(
                        resident,
                        kithara_render::ServiceClass::Warm,
                    )],
                },
            )
            .expect("fill dispatcher credits");
    }
    assert_eq!(rig.dispatcher.available(), 0);
    let settled = answer_source(&mut track, &mut rig, receipt);
    assert!(matches!(
        settled,
        Settled::Rejected {
            reason: Rejection::Refused(PlayError::Full("deck")),
            ..
        }
    ));
    assert_eq!(track.status(), TrackStatus::Idle);

    rig.opens.drain();
    while let Some(due) = rig.opens.next_due((), 1) {
        assert!(matches!(
            due.commands(),
            [DispatcherCommand::SetPriority(_, _)]
        ));
        due.apply(Dispatched::Prioritized);
    }
    let replies: Vec<_> = rig.dispatcher.receipts().collect();
    for reply in replies {
        answer_source(&mut track, &mut rig, reply);
    }
    assert!(rig.dispatcher.available() > 0);
    if matches!(drive, RetirementDrive::NewLoad) {
        assert!(
            matches!(
                in_pass(&mut rig, frame(0), |out| track.apply(
                    TrackCommand::Load {
                        item: item(),
                        position: Position::ZERO
                    },
                    out,
                )),
                Err(PlayError::NotReady)
            ),
            "a new source cannot overwrite a retiring resident"
        );
        return;
    }

    if matches!(drive, RetirementDrive::Explicit) {
        rig.with_outbox(|out| track.release_lane(out))
            .expect("live scope")
            .expect("explicit source retirement");
    } else {
        in_pass(&mut rig, frame(0), |out| track.tick(frame(0), out));
    }
    rig.opens.drain();
    let mut released = false;
    while let Some(due) = rig.opens.next_due((), 1) {
        if matches!(due.commands(), [DispatcherCommand::Release(id)] if *id == resident) {
            released = true;
            drop(inbox.take());
            due.apply(Dispatched::Released);
        } else {
            assert!(matches!(
                due.commands(),
                [DispatcherCommand::SetPriority(_, _)]
            ));
            due.apply(Dispatched::Prioritized);
        }
    }
    assert!(released, "Idle does not suppress a resident release retry");
    let replies: Vec<_> = rig.dispatcher.receipts().collect();
    for reply in replies {
        answer_source(&mut track, &mut rig, reply);
    }
    let _ = rig.block(frame(0), 0.0);
    while track.lane_receipt().is_some() {}
    load(&mut track, &mut rig, Position::ZERO);
    let opened = opened_fixture(&mut rig, "reloaded").await;
    let receipt = rig.open(Ok(opened)).expect("fresh opened source");
    answer_source(&mut track, &mut rig, receipt);
    let mut replies = rig.block(frame(0), 0.0);
    settle(&mut track, &mut rig, &mut replies);
    assert_eq!(track.status(), TrackStatus::Loaded);
    assert_eq!(track.entry(Bound::AtOrAfter(frame(0))), Some(frame(0)));
}

#[kithara::test]
#[case::before_tick(false)]
#[case::after_tick(true)]
fn lane_retirement_failed_open_cannot_latch_an_old_preparation_failure(
    #[case] tick_before_refusal: bool,
) {
    let mut rig = rig();
    let mut track = track(A);
    let loading = load(&mut track, &mut rig, Position::ZERO);
    let requested = command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Seek { to: Position::ZERO },
    )
    .expect("source request while opening");
    rig.opens.drain();
    let mut opening = rig.opens.next_due((), 1).expect("opening source");
    assert_eq!(opening.seq(), loading);
    let [DispatcherCommand::Load(request)] = opening.commands_mut().as_mut_slice() else {
        panic!("one open request");
    };
    request.inbox.drain();
    let due = request
        .inbox
        .next_due(
            LaneFrame {
                segment: SegmentId::FIRST,
                frame: 0,
            },
            1,
        )
        .expect("source preparation");
    assert_eq!(due.seq(), requested);
    due.refuse(kithara_audio::TrackFailureKind::SourceCancelled);
    opening.defer();
    assert!(matches!(
        in_pass(&mut rig, frame(0), |out| track.prepare_entry(frame(0), out)),
        Err(PlayError::Lane(
            kithara_audio::TrackFailureKind::SourceCancelled
        ))
    ));
    assert_eq!(track.status(), TrackStatus::Loading);
    if tick_before_refusal {
        in_pass(&mut rig, frame(0), |out| track.tick(frame(0), out));
        assert!(matches!(track.status(), TrackStatus::Failed { .. }));
    }
    rig.opens
        .resume(loading, (), ())
        .expect("matching deferred open")
        .refuse(LoadRefusal::Cancelled);
    let receipt = rig.dispatcher.receipts().next().expect("open refusal");
    answer_source(&mut track, &mut rig, receipt);
    assert_eq!(track.status(), TrackStatus::Idle);
    in_pass(&mut rig, frame(0), |out| track.tick(frame(0), out));
    assert_eq!(
        track.status(),
        TrackStatus::Idle,
        "retired preparation cannot fail an idle source"
    );
}

#[kithara::test]
fn lane_retirement_late_receipts_do_not_reopen_or_reconfigure_the_scope() {
    let mut rig = rig();
    let mut track = track(A);
    let loading = load(&mut track, &mut rig, Position::ZERO);
    let requested = command_at_zero(
        &mut track,
        &mut rig,
        TrackCommand::Configure(TrackSettingsChange::Keylock(true), When::Next),
    )
    .expect("pending configuration");
    command_at_zero(&mut track, &mut rig, TrackCommand::Release);
    let latency = track.snapshot().engine_latency;
    rig.opens.drain();
    let mut opening = rig.opens.next_due((), 1).expect("opening source");
    assert_eq!(opening.seq(), loading);
    let [DispatcherCommand::Load(request)] = opening.commands_mut().as_mut_slice() else {
        panic!("one open request");
    };
    request.inbox.drain();
    let due = request
        .inbox
        .next_due(
            LaneFrame {
                segment: SegmentId::FIRST,
                frame: 0,
            },
            1,
        )
        .expect("pending old configuration");
    assert_eq!(due.seq(), requested);
    due.apply(kithara_render::LaneApplied {
        source_basis: None,
        engine_latency: FrameCount::new(123),
        ready: Some(SegmentId::FIRST),
    });
    opening.defer();
    in_pass(&mut rig, frame(0), |out| track.tick(frame(0), out));
    assert_eq!(
        track.snapshot().engine_latency,
        latency,
        "closed-scope receipts cannot change source facts"
    );
    assert!(!track.snapshot().pending_lane);
    assert!(
        command_at_zero(
            &mut track,
            &mut rig,
            TrackCommand::Configure(TrackSettingsChange::Keylock(false), When::Next)
        )
        .is_none()
    );
    assert!(!track.projected().keylock());
    rig.opens
        .resume(loading, (), ())
        .expect("retiring open")
        .refuse(LoadRefusal::Cancelled);
    let receipt = rig
        .dispatcher
        .receipts()
        .next()
        .expect("terminal open refusal");
    answer_source(&mut track, &mut rig, receipt);
    assert_eq!(track.status(), TrackStatus::Released);
    assert!(!track.projected().keylock());
}
