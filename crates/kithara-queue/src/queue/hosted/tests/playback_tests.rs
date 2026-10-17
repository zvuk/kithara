use kithara_platform::time::Duration;
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
                    position: kithara_play::Position::ZERO,
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
    assert_eq!(queue.current, None, "navigation must not have advanced");
}

#[kithara::test(tokio)]
async fn eof_after_queue_end_does_not_restart_from_first_track() {
    let (mut queue, _first, second, mut rig, dir) = crate::mock::selected().await;
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
    crate::mock::finish(&mut queue, &mut rig);
    queue.current = Some(second);
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
    assert_eq!(queue.current, None, "stale EOF must not restart the queue");
    let saw_ended =
        wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 200).await;
    assert!(!saw_ended, "stale EOF must not duplicate QueueEnded");
}

#[kithara::test(tokio)]
#[kithara::flash(io)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn play_retries_the_current_track_after_its_prefetch_failed() {
    use std::{future::Future, task::Poll};

    use kithara_command::{Batch, ChannelConfig, Outcome, Rejection, channel};
    use kithara_play::mock;
    use kithara_render::{Dispatched, DispatcherCommand, LoadRefusal};
    use kithara_signal::AudioSpec;

    let (mut queue, mut rig, dir) = empty_queue();
    let id = TrackId::allocate();
    let path = dir.path().join("retry.wav");
    let source = path.to_str().expect("fixture path").to_owned();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Append {
                position: kithara_play::Position::ZERO,
                id,
                source: TrackSource::Uri(source.clone()),
            },
            out,
        )
        .expect("open queue accepts a track");
    });
    rig.opens.drain();
    let mut due = rig.opens.next_due((), 1).expect("initial source load");
    let failed_seq = due.seq();
    let command = due.commands_mut().pop().expect("one source load");
    assert!(matches!(command, DispatcherCommand::Load(_)));
    assert!(due.commands().is_empty());
    due.defer();
    let refusal = {
        let _io = kithara_platform::flash::real_io();
        let (mut sender, inbox) = channel(ChannelConfig::builder().build());
        sender
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![command],
                },
            )
            .expect("actual dispatcher accepts the source load");
        let mut dispatcher = std::pin::pin!(kithara_render::dispatch(inbox));
        let receipt = std::future::poll_fn(|context| {
            hang_tick!();
            let _ = dispatcher.as_mut().poll(context);
            sender.receipts().next().map_or_else(
                || Poll::Pending,
                |receipt| {
                    hang_reset!();
                    Poll::Ready(receipt)
                },
            )
        })
        .await;
        let (outcome, _) = receipt.into();
        let Outcome::Rejected(Rejection::Refused(refusal @ LoadRefusal::Open(_))) = outcome else {
            panic!("the unavailable source must fail to open: {outcome:?}");
        };
        refusal
    };
    rig.opens
        .resume(failed_seq, (), ())
        .expect("initial owner load")
        .refuse(refusal);
    let receipt = rig
        .dispatcher
        .receipts()
        .next()
        .expect("actual load refusal");
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::settle(queue, TrackReceipt::Loaded(receipt), out)
    });
    assert!(matches!(
        queue.track(id).expect("retained source").status,
        TrackStatus::Failed(_)
    ));
    for _ in 0..3 {
        tick_fixture(&mut queue, &mut rig);
    }
    rig.opens.drain();
    loop {
        let Some(due) = rig.opens.next_due((), 1) else {
            break;
        };
        hang_tick!();
        match due.commands() {
            [DispatcherCommand::Release(_)] => due.apply(Dispatched::Released),
            [DispatcherCommand::SetPriority(_, _)] => due.apply(Dispatched::Prioritized),
            _ => panic!("a failed source must not reopen before an explicit Play"),
        }
        let receipt = rig.dispatcher.receipts().next().expect("cleanup receipt");
        with_outbox(&mut queue, &mut rig, |queue, out| {
            Player::settle(queue, TrackReceipt::Loaded(receipt), out)
        });
        hang_reset!();
    }
    std::fs::copy(dir.path().join("entry.wav"), &path).expect("restore the requested source");
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(queue, QueueCommand::Play { at: When::Next }, out)
    })
    .expect("retry failed prefetch");
    let pending = queue
        .target
        .expect("play must retain selection while retrying the failed track");
    assert_eq!(pending.to, id);
    assert_eq!(pending.playing, true);
    let retry_seq = queue
        .incoming_index(id)
        .and_then(|index| queue.active.get(index))
        .and_then(|active| active.load)
        .expect("Play opens the retained source again")
        .seq();
    assert_ne!(retry_seq, failed_seq, "Play must start a new open");
    let loaded =
        crate::mock::loaded_request(queue.worker().expect("fixture worker"), &mut rig, retry_seq)
            .await;
    assert_eq!(loaded.opened.pcm.src().as_ref(), source);
    assert_eq!(
        loaded.opened.pcm.spec(),
        AudioSpec::new(2, mock::SAMPLE_RATE)
    );
    assert!(
        loaded
            .opened
            .pcm
            .next_chunk_info()
            .and_then(|info| info.media_end)
            .is_some_and(|duration| !duration.is_zero())
    );
    answer_loaded(&mut queue, &mut rig, loaded);
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(queue.current, Some(id));
    assert_eq!(
        queue.track(id).expect("played source").status,
        TrackStatus::Consumed
    );
    assert!(queue.target.is_none());
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
                QueueCommand::Append {
                    id,
                    source,
                    position: kithara_play::Position::ZERO,
                }
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
                && matches!(active.role, Role::Incoming)
                && matches!(
                    active.load,
                    Some(crate::queue::slots::LoadState::Opening(_))
                )
        })
}
