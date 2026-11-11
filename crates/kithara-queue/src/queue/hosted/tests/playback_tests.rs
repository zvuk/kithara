use kithara_platform::time::Duration;
use kithara_play::{DeckEvent, Slot};

use super::*;
use crate::{Transition, mock::fixture_uri, queue::state::tests::wait_for_queue_event};

#[kithara::test(tokio)]
async fn spurious_item_did_play_to_end_is_filtered(#[by_ref] worker_owner: &WorkerOwner) {
    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    with_outbox(&mut queue, &mut rig, |queue, out| {
        for _ in 0..2 {
            Player::apply(
                queue,
                QueueCommand::Append {
                    position: kithara_play::Position::ZERO,
                    id: TrackId::allocate(),
                    source: TrackSource::Uri(fixture_uri(dir.path(), "entry.wav")),
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
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn eof_after_queue_end_does_not_restart_from_first_track(
    #[by_ref] worker_owner: &WorkerOwner,
) {
    let (mut queue, _first, second, mut rig, dir) = crate::mock::selected(&worker_owner.0).await;
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
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
#[case(false)]
#[case(true)]
async fn replay_after_queue_end_restores_current_and_reports_the_next_end(
    #[by_ref] worker_owner: &WorkerOwner,
    #[case] seek_first: bool,
) {
    let (mut queue, first, second, mut rig, fixture_directory) =
        crate::mock::selected(&worker_owner.0).await;
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(queue, QueueCommand::Remove(second), out)
    })
    .expect("retain only the sounding track");
    let slot = queue
        .active
        .iter()
        .find(|active| active.item == first && active.role == Role::Current)
        .expect("sounding track")
        .slot;
    let mut events = queue.subscribe::<QueueEvent>();
    rig.mixer
        .report(DeckEvent::Ended {
            slot,
            at: SessionFrame::new(512),
        })
        .expect("first playback end");
    tick_fixture(&mut queue, &mut rig);
    assert_eq!(queue.current, None);
    assert_eq!(
        std::iter::from_fn(|| events.try_recv().ok())
            .filter(|envelope| matches!(envelope.event, QueueEvent::QueueEnded))
            .count(),
        1
    );

    if seek_first {
        with_outbox(&mut queue, &mut rig, |queue, out| {
            Player::apply(
                queue,
                QueueCommand::Seek {
                    to: kithara_play::Position::ZERO,
                },
                out,
            )
        })
        .expect("accept explicit rewind");
        assert_eq!(queue.current, None);
    }
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(queue, QueueCommand::Play { at: When::Next }, out)
    })
    .expect("accept replay");
    assert_eq!(
        queue.current, None,
        "replay must wait for its applied receipt"
    );
    for receipt in rig
        .block(SessionFrame::new(768), 0.0)
        .expect("replay adoption")
    {
        let seq = receipt.seq();
        let (outcome, mut batch) = receipt.into();
        with_outbox(&mut queue, &mut rig, |queue, out| {
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
    assert_eq!(queue.current, Some(first));
    let replay = std::iter::from_fn(|| events.try_recv().ok())
        .map(|envelope| envelope.event)
        .collect::<Vec<_>>();
    assert!(
        matches!(replay.iter().filter(|event| matches!(event,
        QueueEvent::CurrentTrackChanged { .. })).collect::<Vec<_>>().as_slice(),
        [QueueEvent::CurrentTrackChanged { id: Some(id) }] if *id == first),
        "replay must restore the retained identity once: {replay:?}"
    );
    assert!(replay.iter().all(|event| !matches!(
        event,
        QueueEvent::CurrentTrackAdvance { .. } | QueueEvent::QueueEnded
    )));

    rig.mixer
        .report(DeckEvent::Ended {
            slot,
            at: SessionFrame::new(1024),
        })
        .expect("second playback end");
    tick_fixture(&mut queue, &mut rig);
    assert_eq!(queue.current, None);
    assert_eq!(
        std::iter::from_fn(|| events.try_recv().ok())
            .filter(|envelope| matches!(envelope.event, QueueEvent::QueueEnded))
            .count(),
        1
    );
    rig.mixer
        .report(DeckEvent::Ended {
            slot,
            at: SessionFrame::new(1024),
        })
        .expect("stale second end");
    tick_fixture(&mut queue, &mut rig);
    assert_eq!(queue.current, None);
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .all(|envelope| !matches!(envelope.event, QueueEvent::QueueEnded))
    );
    drop((queue, rig));
    fixture_directory.finish().await;
}

#[kithara::test(tokio)]
#[kithara::flash(io)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn play_retries_the_current_track_after_its_prefetch_failed(
    #[by_ref] worker_owner: &WorkerOwner,
) {
    use std::{future::Future, task::Poll};

    use kithara_command::{Batch, ChannelConfig, Outcome, Rejection, channel};
    use kithara_play::mock;
    use kithara_render::{Dispatched, DispatcherCommand, LoadRefusal};
    use kithara_signal::AudioSpec;

    let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
    let id = TrackId::allocate();
    let source = fixture_uri(dir.path(), "retry.wav");
    let store = queue.config.store.as_ref().expect("fixture store").clone();
    crate::mock::seed_fixture(&store, &source, b"not a WAV source");
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
    store
        .remove_resource(&crate::mock::fixture_key(&store, &source))
        .expect("remove malformed fixture");
    crate::mock::seed_fixture(
        &store,
        &source,
        kithara_test_fixtures::assets::fixed_constant_wav_cd_half_1024().bytes(),
    );
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
    drop((queue, rig));
    dir.finish().await;
}

#[kithara::test(tokio)]
async fn play_promotes_the_initial_pending_prefetch(#[by_ref] worker_owner: &WorkerOwner) {
    for insert in [false, true] {
        let (mut queue, mut rig, dir) = empty_queue(&worker_owner.0);
        let id = TrackId::allocate();
        let source = TrackSource::Uri(fixture_uri(dir.path(), "entry.wav"));
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
        drop((queue, rig));
        dir.finish().await;
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
