use kithara_play::{DeckEvent, Slot};

use super::*;
use crate::{Transition, queue::state::tests::wait_for_queue_event};

#[kithara::test(tokio)]
async fn spurious_item_did_play_to_end_is_filtered() {
    let (mut queue, mut rig, dir) = empty_queue();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        for _ in 0..2 {
            Player::apply(
                queue,
                QueueCommand::Append {
                    id: TrackId::allocate(),
                    source: TrackSource::Uri(
                        dir.path()
                            .join("entry.wav")
                            .to_str()
                            .expect("fixture path")
                            .to_owned(),
                    ),
                },
                out,
            )
            .expect("append fixture");
        }
    });
    rig.mixer
        .report(DeckEvent::Ended {
            slot: Slot::new(0),
            at: SessionFrame::new(128),
        })
        .expect("spurious terminal event");
    tick_fixture(&mut queue, &mut rig);
    assert_eq!(
        queue.navigation.current(),
        None,
        "navigation must not have advanced"
    );
}

#[kithara::test(tokio)]
async fn eof_after_queue_end_does_not_restart_from_first_track() {
    let (mut queue, first, second, mut rig, dir) = player_internal::selected().await;
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Select {
                id: second,
                transition: Transition::None,
            },
            out,
        )
    })
    .expect("select last");
    answer_load(&mut queue, &mut rig, &dir).await;
    player_internal::finish(&mut queue, &mut rig);
    queue.navigation.select(second, &[first, second]);
    queue.current = None;
    queue.navigation = crate::NavigationState::new(queue.navigation.history_limit());
    let mut rx = queue.subscribe::<QueueEvent>();
    let slot = queue
        .active
        .iter()
        .find(|active| active.item == second)
        .expect("last slot")
        .slot;
    rig.mixer
        .report(DeckEvent::Ended {
            slot,
            at: SessionFrame::new(512),
        })
        .expect("stale end");
    tick_fixture(&mut queue, &mut rig);
    assert_eq!(
        queue.navigation.current(),
        None,
        "stale EOF must not restart the queue"
    );
    let saw_ended =
        wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 200).await;
    assert!(!saw_ended, "stale EOF must not duplicate QueueEnded");
}

#[kithara::test(tokio)]
async fn play_retries_the_current_track_after_its_prefetch_failed() {
    let (mut queue, mut rig, dir) = empty_queue();
    let id = TrackId::allocate();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Append {
                id,
                source: TrackSource::Uri(
                    dir.path()
                        .join("entry.wav")
                        .to_str()
                        .expect("fixture path")
                        .to_owned(),
                ),
            },
            out,
        )
        .expect("open queue accepts a track");
    });
    queue
        .tracks
        .set_status(id, TrackStatus::Failed("network offline".into()));
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(queue, QueueCommand::Play { at: When::Next }, out)
    })
    .expect("retry failed prefetch");
    let pending = queue
        .target
        .expect("play must retain selection while retrying the failed track");
    assert_eq!(pending.to, id);
    assert_eq!(pending.playing, true);
}

#[kithara::test(tokio)]
async fn play_promotes_the_initial_pending_prefetch() {
    for insert in [false, true] {
        let (mut queue, mut rig, dir) = empty_queue();
        let id = TrackId::allocate();
        let source = TrackSource::Uri(
            dir.path()
                .join("entry.wav")
                .to_str()
                .expect("fixture path")
                .to_owned(),
        );
        with_outbox(&mut queue, &mut rig, |queue, out| {
            let command = if insert {
                QueueCommand::Insert {
                    id,
                    source,
                    after: None,
                }
            } else {
                QueueCommand::Append { id, source }
            };
            Player::apply(queue, command, out).expect("open queue accepts a track");
        });
        tick_fixture(&mut queue, &mut rig);
        assert!(!attempt_selected(&queue, id));
        with_outbox(&mut queue, &mut rig, |queue, out| {
            Player::apply(queue, QueueCommand::Play { at: When::Next }, out)
        })
        .expect("promote initial prefetch");
        assert!(attempt_selected(&queue, id));
    }
}

fn attempt_selected(queue: &Queue<TestPools>, id: TrackId) -> bool {
    queue
        .target
        .is_some_and(|target| target.to == id && target.playing)
        && queue.active.iter().any(|active| {
            active.item == id
                && matches!(active.role, Role::Incoming { .. })
                && matches!(
                    active.load,
                    Some(crate::queue::slots::LoadState::Opening(_))
                )
        })
}
