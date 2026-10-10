use kithara_audio::{DecodeErrorKind, TrackFailureKind};
use kithara_play::{DeckEvent, PlaybackFault, Slot};

use super::*;
use crate::{AdvanceReason, queue::state::tests::wait_for_queue_event};

#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn pause_and_none_suppress_natural_eof_progression() {
    for action in [ActionAtItemEnd::Pause, ActionAtItemEnd::None] {
        let (mut queue, first, _second, mut rig, _dir) = player_internal::selected().await;
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
    let (mut queue, first, _, mut rig, _dir) = player_internal::selected().await;
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
    assert!(
        queue
            .current_track()
            .is_some_and(|track| matches!(track.snapshot().status, PlayingStatus::Paused { .. }))
    );
}

#[kithara::test(tokio)]
async fn the_deck_leading_on_past_a_failed_track_advances_for_the_failure() {
    let (mut queue, failed, successor, mut rig, dir) = player_internal::selected().await;
    let slot = queue
        .active
        .iter()
        .find(|active| active.item == failed)
        .expect("failed slot")
        .slot;
    let mut events = queue.subscribe::<QueueEvent>();
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
    answer_load(&mut queue, &mut rig, &dir).await;
    player_internal::finish(&mut queue, &mut rig);
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
    let (mut queue, last, _, mut rig, _dir) = player_internal::selected().await;
    queue.navigation.select(last, &queue.track_ids());
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
async fn lagged_player_events_resynchronize_current_track() {
    let (mut queue, id, _, mut rig, _dir) = player_internal::selected().await;
    let unrelated = DeckEvent::Ended {
        slot: Slot::new(3),
        at: SessionFrame::new(128),
    };
    while rig.mixer.report(unrelated).is_ok() {}
    rig.events.drain().for_each(drop);
    let mut events = queue.subscribe::<QueueEvent>();
    tick_fixture(&mut queue, &mut rig);
    let saw_current_track = wait_for_queue_event(&mut events, |event| {
        matches!(event, QueueEvent::CurrentTrackChanged { id: Some(current_id) } if *current_id == id)
    }, 200).await;
    assert!(
        saw_current_track,
        "lag recovery should re-announce the current track"
    );
}
