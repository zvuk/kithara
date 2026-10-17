use kithara_audio::{DecodeErrorKind, TrackFailureKind};
use kithara_command::Port;
use kithara_events::{DEFAULT_EVENT_BUS_CAPACITY, SlotId};
use kithara_play::{DeckEvent, ItemRole, PlaybackFault, PlayerEvent, Slot, TrackRef};
use kithara_signal::SegmentId;

use super::*;
use crate::{AdvanceReason, queue::state::tests::wait_for_queue_event};

#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn pause_and_none_suppress_natural_eof_progression() {
    for action in [ActionAtItemEnd::Pause, ActionAtItemEnd::None] {
        hang_tick!();
        let (mut queue, first, _second, mut rig, _dir) = crate::mock::selected().await;
        hang_reset!();
        with_outbox(&mut queue, &mut rig, |queue, out| {
            Player::apply(queue, QueueCommand::SetActionAtItemEnd(action), out)
        })
        .expect("end action");
        let slot = queue
            .active
            .iter()
            .find(|active| active.item == first)
            .expect("current slot")
            .slot;
        let mut events = queue.subscribe::<QueueEvent>();
        rig.mixer
            .report(DeckEvent::Ended {
                slot,
                at: SessionFrame::new(1024),
            })
            .expect("natural end");
        tick_fixture(&mut queue, &mut rig);
        assert_eq!(queue.current().map(|entry| entry.id), Some(first));
        assert!(queue.target.is_none());
        if action == ActionAtItemEnd::Pause {
            settle_natural_end_pause(&mut queue, &mut rig, slot);
            assert!(queue.current_track().is_some_and(|track| matches!(
                track.snapshot().status,
                PlayingStatus::Paused { .. }
            )));
        }
        assert!(
            !wait_for_queue_event(
                &mut events,
                |event| matches!(event, QueueEvent::QueueEnded),
                50
            )
            .await
        );
    }
}

#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn a_tick_pauses_at_the_natural_end_it_drains() {
    let (mut queue, first, _, mut rig, _dir) = crate::mock::selected().await;
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::SetActionAtItemEnd(ActionAtItemEnd::Pause),
            out,
        )
    })
    .expect("pause at end");
    let slot = queue
        .active
        .iter()
        .find(|active| active.item == first)
        .expect("current slot")
        .slot;
    rig.mixer
        .report(DeckEvent::Ended {
            slot,
            at: SessionFrame::new(1024),
        })
        .expect("natural end");
    tick_fixture(&mut queue, &mut rig);
    settle_natural_end_pause(&mut queue, &mut rig, slot);
    assert!(
        queue
            .current_track()
            .is_some_and(|track| matches!(track.snapshot().status, PlayingStatus::Paused { .. }))
    );
}

fn settle_natural_end_pause(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    slot: Slot,
) {
    let current = queue.active_current_index().expect("ended current slot");
    let ended = PlayingStatus::Ended {
        at: SessionFrame::new(1024),
    };
    assert_eq!(
        queue
            .active
            .get(current)
            .expect("ended current track")
            .track
            .snapshot()
            .status,
        ended
    );
    let pause = rig
        .ring
        .scope(rig.scope)
        .expect("deck scope")
        .basis(slot, When::Next)
        .expect("native Pause sequence");
    let stopped_mark = SlotMark {
        session: SessionFrame::new(2048),
        lane: LaneFrame {
            segment: SegmentId::FIRST,
            frame: 0,
        },
        position: Duration::ZERO,
    };
    let mut stopped = rig
        .block(SessionFrame::new(2048), 0.0)
        .expect("native Stop block");
    assert_eq!(stopped.len(), 1);
    let receipt = stopped.pop().expect("native Stop receipt");
    assert_eq!(receipt.seq(), pause);
    let (outcome, mut batch) = receipt.into();
    assert!(matches!(outcome, Outcome::Applied { at, .. } if at == SessionFrame::new(2048)));
    assert!(matches!(
        batch.commands.as_slice(),
        [DeckPart::Returned(Returned::Stopped { slot: stopped_slot, resume: Some(mark) })]
            if *stopped_slot == slot && *mark == stopped_mark
    ));
    assert_eq!(
        queue
            .active
            .get(current)
            .expect("unsettled current track")
            .track
            .snapshot()
            .status,
        ended
    );
    with_outbox(queue, rig, |queue, out| {
        Player::settle(
            queue,
            TrackReceipt::Deck {
                seq: pause,
                outcome: &outcome,
                batch: &mut batch,
            },
            out,
        )
    });
    assert_eq!(
        queue
            .current_track()
            .expect("stopped current track")
            .snapshot()
            .position,
        stopped_mark.position
    );
}

#[kithara::test(tokio)]
async fn the_deck_leading_on_past_a_failed_track_advances_for_the_failure() {
    let (mut queue, failed, successor, mut rig, dir) = crate::mock::selected().await;
    let slot = queue
        .active
        .iter()
        .find(|active| active.item == failed)
        .expect("failed slot")
        .slot;
    let mut events = queue.subscribe::<QueueEvent>();
    let mut observations = queue.subscribe::<PlayerEvent>();
    rig.mixer
        .report(DeckEvent::Failed {
            slot,
            at: SessionFrame::new(1024),
            fault: PlaybackFault::Source(TrackFailureKind::Decode {
                kind: DecodeErrorKind::InvalidData,
            }),
        })
        .expect("source failure");
    tick_fixture(&mut queue, &mut rig);
    assert!(matches!(
        queue.track(failed).expect("failed entry").status,
        TrackStatus::Failed(_)
    ));
    assert!(matches!(
        observations.try_recv().expect("failure observation").event,
        PlayerEvent::ItemDidFail { item: ItemRole::Leading(track), .. } if track.id == failed
    ));
    for _ in 0..=DEFAULT_EVENT_BUS_CAPACITY {
        queue
            .bus
            .publish(PlayerEvent::VolumeChanged { volume: 0.5 });
    }
    assert!(matches!(
        observations.try_recv(),
        Err(kithara_platform::tokio::sync::broadcast::error::TryRecvError::Lagged(_))
    ));
    tick_fixture(&mut queue, &mut rig);
    answer_load(&mut queue, &mut rig, &dir).await;
    crate::mock::finish(&mut queue, &mut rig);
    let advanced = wait_for_queue_event(
        &mut events,
        |event| {
            matches!(event, QueueEvent::CurrentTrackAdvance {
            reason: AdvanceReason::TrackFailed, id: Some(id),
        } if *id == successor)
        },
        200,
    )
    .await;
    assert!(
        advanced,
        "the cursor leaves the failed track for its failure"
    );
}

#[kithara::test(tokio)]
async fn lag_recovery_keeps_an_ended_queue_ended() {
    let (mut queue, last, _, mut rig, _dir) = crate::mock::selected().await;
    queue.current = Some(last);
    queue.current = None;
    queue.navigation = crate::NavigationState::new(queue.navigation.history_limit());
    queue.publish();
    let mut events = queue.subscribe::<QueueEvent>();
    tick_fixture(&mut queue, &mut rig);
    assert_eq!(queue.current().map(|entry| entry.id), None);
    assert!(
        !wait_for_queue_event(
            &mut events,
            |event| matches!(event, QueueEvent::CurrentTrackAdvance { .. }),
            50
        )
        .await,
        "the ended queue advances nowhere"
    );
}

#[kithara::test(tokio)]
async fn overflowing_player_observations_do_not_announce_an_unchanged_current_track() {
    let (mut queue, id, _, mut rig, _dir) = crate::mock::selected().await;
    let mut observations = queue.subscribe::<PlayerEvent>();
    let mut events = queue.subscribe::<QueueEvent>();
    for _ in 0..=DEFAULT_EVENT_BUS_CAPACITY {
        queue
            .bus
            .publish(PlayerEvent::VolumeChanged { volume: 0.5 });
    }
    assert!(matches!(
        observations.try_recv(),
        Err(kithara_platform::tokio::sync::broadcast::error::TryRecvError::Lagged(_))
    ));
    tick_fixture(&mut queue, &mut rig);
    assert_eq!(queue.current().map(|entry| entry.id), Some(id));
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .all(|envelope| !matches!(envelope.event, QueueEvent::CurrentTrackChanged { .. }))
    );
}

#[kithara::test(tokio)]
async fn a_failure_observation_cannot_fail_the_current_track() {
    let (mut queue, id, _, mut rig, _dir) = crate::mock::selected().await;
    let slot = queue
        .active
        .iter()
        .find(|active| active.item == id)
        .expect("current slot")
        .slot;
    let before = queue.track(id).expect("current entry").status;
    let mut events = queue.subscribe::<QueueEvent>();
    queue.bus.publish(PlayerEvent::ItemDidFail {
        item: ItemRole::Leading(TrackRef::new(
            id,
            SlotId::new(u64::from(slot.get())),
            "observation.wav".into(),
        )),
        fault: PlaybackFault::Source(TrackFailureKind::Decode {
            kind: DecodeErrorKind::InvalidData,
        }),
    });
    tick_fixture(&mut queue, &mut rig);
    assert_eq!(queue.current().map(|entry| entry.id), Some(id));
    assert_eq!(queue.track(id).expect("current entry").status, before);
    assert!(queue.target.is_none());
    assert!(
        std::iter::from_fn(|| events.try_recv().ok()).all(|envelope| !matches!(
            envelope.event,
            QueueEvent::TrackLoadFailed { .. }
                | QueueEvent::CurrentTrackAdvance { .. }
                | QueueEvent::CurrentTrackChanged { .. }
        ))
    );
}
