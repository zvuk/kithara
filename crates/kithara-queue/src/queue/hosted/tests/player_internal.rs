use kithara_events::EventReceiver;
use kithara_play::{PlayerStatus, mock};

use super::*;
use crate::{QueueError, Transition};

fn apply(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    command: QueueCommand<TestPools>,
) -> Result<Option<Seq>, PlayError> {
    with_outbox(queue, rig, |queue, out| Player::apply(queue, command, out))
}

pub(in crate::queue) fn settle(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    at: SessionFrame,
) {
    for receipt in rig.block(at, 0.0).expect("deck block") {
        let seq = receipt.seq();
        let (outcome, mut batch) = receipt.into();
        with_outbox(queue, rig, |queue, out| {
            Player::settle(
                queue,
                TrackReceipt::Deck {
                    seq,
                    outcome: &outcome,
                    batch: &mut batch,
                },
                out,
            )
        });
    }
}

pub(in crate::queue) fn finish(queue: &mut Queue<TestPools>, rig: &mut mock::DeckRig<TestPools>) {
    for at in [0, 128, 256] {
        settle(queue, rig, SessionFrame::new(at));
    }
}

pub(super) async fn selected() -> (
    Queue<TestPools>,
    TrackId,
    TrackId,
    mock::DeckRig<TestPools>,
    TestTempDir,
) {
    let (mut queue, first, second, mut rig, dir) = pending_selection();
    answer_load(&mut queue, &mut rig, &dir).await;
    finish(&mut queue, &mut rig);
    (queue, first, second, rig, dir)
}

async fn pending_successor() -> (
    Queue<TestPools>,
    TrackId,
    TrackId,
    mock::DeckRig<TestPools>,
    TestTempDir,
) {
    let (mut queue, first, second, mut rig, dir) = selected().await;
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: second,
            transition: Transition::None,
        },
    )
    .expect("select successor");
    (queue, first, second, rig, dir)
}

fn drain(rx: &mut EventReceiver<QueueEvent>) -> Vec<QueueEvent> {
    std::iter::from_fn(|| rx.try_recv().ok().map(|envelope| envelope.event)).collect()
}

#[kithara::test(tokio)]
#[case(false)]
#[case(true)]
async fn player_remove_all_resets_state(#[case] with_item: bool) {
    let (mut queue, _, _, mut rig, dir) = pending_selection();
    if with_item {
        apply(&mut queue, &mut rig, QueueCommand::Pause { at: When::Next })
            .expect("pause selection");
        answer_load(&mut queue, &mut rig, &dir).await;
        finish(&mut queue, &mut rig);
        assert!(queue.control().current().is_some());
    }
    apply(&mut queue, &mut rig, QueueCommand::RemoveAll).expect("clear");
    finish(&mut queue, &mut rig);
    assert_eq!(queue.current, None);
    assert_eq!(queue.control().status(), PlayerStatus::Unknown);
}

#[kithara::test(tokio)]
async fn replay_same_item_does_not_re_emit_current_item_changed() {
    let (mut queue, _, _, mut rig, dir) = pending_selection();
    let mut rx = queue.control().subscribe();
    answer_load(&mut queue, &mut rig, &dir).await;
    finish(&mut queue, &mut rig);
    let first = drain(&mut rx);
    let first_count = first
        .iter()
        .filter(|event| matches!(event, QueueEvent::CurrentTrackChanged { .. }))
        .count();
    assert_eq!(
        first_count, 1,
        "selecting the item announces it once: {first:?}"
    );
    apply(&mut queue, &mut rig, QueueCommand::Play { at: When::Next }).expect("resume");
    finish(&mut queue, &mut rig);
    let second = drain(&mut rx);
    let second_count = second
        .iter()
        .filter(|event| matches!(event, QueueEvent::CurrentTrackChanged { .. }))
        .count();
    assert_eq!(
        second_count, 0,
        "resuming the same item must not re-announce CurrentItemChanged: {second:?}"
    );
}

#[kithara::test(tokio)]
async fn a_selected_resource_adopts_the_session_wake_mode() {
    let pools = pools();
    let dir = TestTempDir::new();
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(
            PlayWorkerConfig::builder(pools.clone()).build(),
        ))
        .build();
    mock::assert_prepared_render_off_bus(
        &prep,
        &mock::output(None).get(),
        &pools,
        &dir.path().join("selected.wav"),
    )
    .await
    .expect("selected lane renders off the bus");
}

#[kithara::test(tokio)]
async fn re_selecting_the_current_item_does_not_re_announce() {
    let (mut queue, item, _, mut rig, _dir) = selected().await;
    let mut rx = queue.control().subscribe();
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: item,
            transition: Transition::None,
        },
    )
    .expect("re-select current");
    apply(&mut queue, &mut rig, QueueCommand::Pause { at: When::Next }).expect("pause current");
    finish(&mut queue, &mut rig);
    let after = drain(&mut rx);
    let announces = after
        .iter()
        .filter(|event| matches!(event, QueueEvent::CurrentTrackChanged { .. }))
        .count();
    assert_eq!(
        announces, 0,
        "re-selecting the current item must not re-announce: {after:?}"
    );
}

#[kithara::test(tokio)]
async fn selecting_another_item_announces_it() {
    let (mut queue, _, second, mut rig, dir) = selected().await;
    let mut rx = queue.control().subscribe();
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: second,
            transition: Transition::None,
        },
    )
    .expect("select second");
    answer_load(&mut queue, &mut rig, &dir).await;
    finish(&mut queue, &mut rig);
    let after = drain(&mut rx);
    assert!(
        matches!(after.iter().filter(|event| matches!(event, QueueEvent::CurrentTrackChanged { .. })).collect::<Vec<_>>().as_slice(),
        [QueueEvent::CurrentTrackChanged { id: Some(announced) }] if *announced == second),
        "selecting another item must announce it once: {after:?}"
    );
    assert_eq!(queue.current, Some(second));
}

#[kithara::test(tokio)]
async fn arm_next_arms_the_item() {
    let (queue, _, second, _rig, _dir) = pending_successor().await;
    assert_eq!(queue.target.map(|target| target.to), Some(second));
}

#[kithara::test(tokio)]
async fn seek_seconds_updates_position_optimistically() {
    let (mut queue, _, _, mut rig, _dir) = pending_selection();
    apply(&mut queue, &mut rig, QueueCommand::RemoveAll).expect("empty queue");
    let to = Duration::from_secs_f64(54.689_879_542);
    let outcome = apply(&mut queue, &mut rig, QueueCommand::Seek { to });
    assert!(matches!(outcome, Ok(None)));
    assert_eq!(queue.control().position_seconds(), Some(54.689_879_542));
}

#[kithara::test(tokio)]
async fn arm_next_idempotent_for_the_armed_item() {
    let (mut queue, _, second, mut rig, _dir) = pending_successor().await;
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: second,
            transition: Transition::None,
        },
    )
    .expect("select same target");
    assert_eq!(queue.target.map(|target| target.to), Some(second));
}

#[kithara::test(tokio)]
async fn arm_next_replaces_a_previously_armed_item() {
    let (mut queue, _, _, mut rig, dir) = pending_successor().await;
    let third = TrackId::allocate();
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Append {
            id: third,
            source: TrackSource::Uri(
                dir.path()
                    .join("entry.wav")
                    .to_str()
                    .expect("path")
                    .to_owned(),
            ),
        },
    )
    .expect("append third");
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: third,
            transition: Transition::None,
        },
    )
    .expect("select third");
    assert_eq!(queue.target.map(|target| target.to), Some(third));
}

#[kithara::test(tokio)]
async fn commit_next_of_another_item_returns_typed_error() {
    let (mut queue, _, second, mut rig, _dir) = pending_successor().await;
    let other = TrackId::allocate();
    let err = with_outbox(&mut queue, &mut rig, |queue, out| {
        queue.apply_command(
            QueueCommand::Select {
                id: other,
                transition: Transition::Crossfade,
            },
            Some(&mock::output(None).get()),
            out,
        )
    })
    .expect_err("mismatch");
    assert!(
        matches!(err, QueueError::UnknownTrackId(requested) if requested == other)
            && queue.target.is_some_and(|target| target.to == second)
    );
}

#[kithara::test(tokio)]
async fn commit_next_makes_the_successor_current_and_announces_it() {
    let (mut queue, _, second, mut rig, dir) = pending_successor().await;
    let mut rx = queue.control().subscribe();
    answer_load(&mut queue, &mut rig, &dir).await;
    finish(&mut queue, &mut rig);
    assert_eq!(queue.current, Some(second));
    assert_eq!(
        queue.target.map(|target| target.to),
        None,
        "armed clears after commit"
    );
    let announced = drain(&mut rx).iter().any(|event| matches!(event, QueueEvent::CurrentTrackChanged { id: Some(item) } if *item == second));
    assert!(announced, "commit_next must publish CurrentItemChanged");
}

#[kithara::test(tokio)]
async fn commit_next_idempotent_when_already_activated() {
    let (mut queue, _, second, mut rig, dir) = pending_successor().await;
    answer_load(&mut queue, &mut rig, &dir).await;
    finish(&mut queue, &mut rig);
    for _ in 0..2 {
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Select {
                id: second,
                transition: Transition::Crossfade,
            },
        )
        .expect("select current");
        finish(&mut queue, &mut rig);
    }
    assert_eq!(queue.current, Some(second));
}

#[kithara::test(tokio)]
async fn unarm_next_clears_an_armed_successor() {
    let (mut queue, first, _, mut rig, _dir) = pending_successor().await;
    with_outbox(&mut queue, &mut rig, Queue::cancel_target).expect("cancel pending transition");
    assert_eq!(queue.target.map(|target| target.to), None);
    assert_eq!(queue.current, Some(first));
}

#[kithara::test(tokio)]
async fn unarm_next_preserves_activated_current() {
    let (mut queue, _, second, mut rig, dir) = pending_successor().await;
    answer_load(&mut queue, &mut rig, &dir).await;
    finish(&mut queue, &mut rig);
    with_outbox(&mut queue, &mut rig, Queue::cancel_target).expect("cancel no pending target");
    assert_eq!(queue.target.map(|target| target.to), None);
    assert_eq!(queue.current, Some(second));
}

#[kithara::test(tokio)]
async fn selecting_another_item_unarms_the_successor() {
    let (mut queue, _, second, mut rig, dir) = pending_successor().await;
    answer_load(&mut queue, &mut rig, &dir).await;
    let third = TrackId::allocate();
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Append {
            id: third,
            source: TrackSource::Uri(
                dir.path()
                    .join("entry.wav")
                    .to_str()
                    .expect("path")
                    .to_owned(),
            ),
        },
    )
    .expect("append third");
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: third,
            transition: Transition::None,
        },
    )
    .expect("select third");
    finish(&mut queue, &mut rig);
    answer_load(&mut queue, &mut rig, &dir).await;
    finish(&mut queue, &mut rig);
    assert_eq!(
        queue.target.map(|target| target.to),
        None,
        "select must unarm"
    );
    assert_eq!(queue.current, Some(third));
    assert_ne!(queue.current, Some(second));
}

#[kithara::test(tokio)]
async fn selecting_the_armed_item_promotes_it() {
    let (mut queue, _, second, mut rig, dir) = pending_successor().await;
    answer_load(&mut queue, &mut rig, &dir).await;
    apply(
        &mut queue,
        &mut rig,
        QueueCommand::Select {
            id: second,
            transition: Transition::None,
        },
    )
    .expect("select armed target");
    finish(&mut queue, &mut rig);
    assert_eq!(queue.current, Some(second));
    assert_eq!(
        queue.target.map(|target| target.to),
        None,
        "armed track consumed by select"
    );
}
