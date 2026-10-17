use kithara_command::Seq;
use kithara_events::TrackId;
use kithara_platform::time::Duration;
use kithara_play::{
    HostedDeck, OpenedTrack, Player, TrackReceipt, TrackStatus as PlayingStatus, mock,
};
use kithara_render::{
    Dispatched, DispatcherCommand, LaneId, Loaded, ServiceClass,
    bridge::{DeckEvent, DeckPart, Fade, FadeDir},
};
use kithara_signal::SessionFrame;
use kithara_test_utils::{TestTempDir, kithara};

use super::super::slots::Role;
use crate::{
    ActionAtItemEnd, Queue, QueueCommand, TrackSource, Transition,
    mock::{TestPools, pending_selection, with_outbox},
};

#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
fn dispatcher(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    mut loaded: Option<Loaded<OpenedTrack>>,
    priorities: &mut Vec<(LaneId, ServiceClass)>,
) {
    loop {
        hang_tick!();
        rig.opens.drain();
        {
            let Some(mut due) = rig.opens.next_due((), 1) else {
                break;
            };
            match due.commands_mut().as_slice() {
                [DispatcherCommand::Load(_)] => {
                    due.defer();
                    hang_reset!();
                    continue;
                }
                [DispatcherCommand::Release(_)] => due.apply(Dispatched::Released),
                [DispatcherCommand::SetPriority(lane, class)] => {
                    priorities.push((*lane, *class));
                    due.apply(Dispatched::Prioritized);
                }
                _ => panic!("one actual dispatcher command per batch"),
            }
        }
        let receipt = rig
            .dispatcher
            .receipts()
            .next()
            .expect("dispatcher receipt");
        with_outbox(queue, rig, |queue, out| {
            Player::settle(queue, TrackReceipt::Loaded(receipt), out)
        });
        hang_reset!();
    }
    if let Some(fixture) = loaded.take() {
        let wanted = queue
            .active
            .iter()
            .filter_map(|active| active.load)
            .last()
            .expect("the actual owner must request the fixture")
            .seq();
        rig.opens
            .resume(wanted, (), ())
            .expect("requested fixture")
            .apply(Dispatched::Loaded(fixture));
        let receipt = rig.dispatcher.receipts().next().expect("fixture receipt");
        with_outbox(queue, rig, |queue, out| {
            Player::settle(queue, TrackReceipt::Loaded(receipt), out)
        });
        hang_reset!();
    }
    assert!(
        loaded.is_none(),
        "the actual owner must request the fixture"
    );
}

#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
fn deck(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    priorities: &mut Vec<(LaneId, ServiceClass)>,
) -> Vec<(kithara_play::Slot, FadeDir)> {
    let mut fades = Vec::new();
    for at in [0, 128, 256] {
        hang_tick!();
        for receipt in rig
            .block(SessionFrame::new(at), 0.0)
            .expect("actual deck batch")
        {
            hang_tick!();
            let seq = receipt.seq();
            let (outcome, mut batch) = receipt.into();
            for part in &batch.commands {
                match part {
                    DeckPart::Fade { slot, dir, .. } => fades.push((*slot, *dir)),
                    DeckPart::Start {
                        slot,
                        fade: Fade::Crossfade(_),
                    } => {
                        fades.push((*slot, FadeDir::In));
                    }
                    DeckPart::Stop {
                        slot,
                        fade: Fade::Crossfade(_),
                    } => {
                        fades.push((*slot, FadeDir::Out));
                    }
                    _ => {}
                }
            }
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
            hang_reset!();
        }
        dispatcher(queue, rig, None, priorities);
        hang_reset!();
    }
    fades
}

#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn open(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    _dir: &TestTempDir,
    priorities: &mut Vec<(LaneId, ServiceClass)>,
    sources: &mut Vec<kithara_render::mock::PacketRing>,
) -> LaneId {
    hang_tick!();
    let seq = queue
        .active
        .iter()
        .filter_map(|active| active.load)
        .last()
        .expect("fixture load")
        .seq();
    let mut loaded =
        crate::mock::loaded_request(queue.worker().expect("fixture worker"), rig, seq).await;
    hang_reset!();
    let source = loaded
        .opened
        .pcm
        .next_chunk_info()
        .expect("prepared fixture source");
    let (opened, packets) = crate::mock::opened_with_source(
        loaded.opened,
        kithara_signal::AudioChunkInfo {
            media_end: Some(Duration::from_secs(10)),
            ..source
        },
    );
    loaded.opened = opened;
    sources.push(packets);
    let lane = loaded.lane;
    dispatcher(queue, rig, Some(loaded), priorities);
    lane
}

fn select_successor(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    item: TrackId,
) -> Option<Seq> {
    with_outbox(queue, rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Select {
                id: item,
                transition: Transition::CrossfadeWith {
                    settings: kithara_play::CrossfadeSettings {
                        duration: 1.0,
                        ..kithara_play::CrossfadeSettings::default()
                    },
                },
            },
            out,
        )
    })
    .expect("crossfade successor")
}

#[kithara::test(tokio)]
async fn track_state_is_leading() {
    let (mut queue, first, second, mut rig, dir) = pending_selection();
    queue.config.action_at_item_end = ActionAtItemEnd::None;
    let mut priorities = Vec::new();
    let mut sources = Vec::new();
    open(&mut queue, &mut rig, &dir, &mut priorities, &mut sources).await;
    deck(&mut queue, &mut rig, &mut priorities);
    assert!(
        queue
            .current_track()
            .is_some_and(|track| track.snapshot().item == first)
    );

    select_successor(&mut queue, &mut rig, second);
    open(&mut queue, &mut rig, &dir, &mut priorities, &mut sources).await;
    let fades = deck(&mut queue, &mut rig, &mut priorities);
    let incoming = queue
        .active
        .iter()
        .find(|active| active.item == second)
        .expect("incoming track");
    let outgoing = queue
        .active
        .iter()
        .find(|active| active.item == first)
        .expect("outgoing track");
    assert!(fades.contains(&(incoming.slot, FadeDir::In)));
    assert!(fades.contains(&(outgoing.slot, FadeDir::Out)));
    assert!(
        queue
            .current_track()
            .is_some_and(|track| track.snapshot().item == second)
    );
    assert!(
        !queue
            .current_track()
            .is_some_and(|track| track.snapshot().item == first)
    );

    let preloaded = TrackId::allocate();
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::apply(
            queue,
            QueueCommand::Append {
                position: kithara_play::Position::ZERO,
                id: preloaded,
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
        .expect("append preload");
        queue
            .load_track(
                preloaded,
                Role::Preloaded,
                Some(&mock::output(None).get()),
                out,
            )
            .expect("actual preload");
    });
    open(&mut queue, &mut rig, &dir, &mut priorities, &mut sources).await;
    deck(&mut queue, &mut rig, &mut priorities);
    assert!(
        !queue
            .current_track()
            .is_some_and(|track| track.snapshot().item == preloaded)
    );

    let finished_slot = queue
        .current_track()
        .expect("finishing track")
        .snapshot()
        .slot
        .expect("finishing track holds a slot");
    with_outbox(&mut queue, &mut rig, |queue, out| {
        Player::settle(
            queue,
            TrackReceipt::Event(DeckEvent::Ended {
                slot: finished_slot,
                at: SessionFrame::new(512),
            }),
            out,
        )
    });
    assert!(matches!(
        queue
            .active
            .iter()
            .find(|active| active.item == second)
            .expect("finished track")
            .track
            .snapshot()
            .status,
        PlayingStatus::Ended { .. }
    ));
    assert!(
        !queue
            .current_track()
            .is_some_and(|track| track.snapshot().item == second)
    );
}

#[kithara::test(tokio)]
#[case("playing", ServiceClass::Audible)]
#[case("fading_in", ServiceClass::Audible)]
#[case("fading_out", ServiceClass::Audible)]
#[case("preloading", ServiceClass::Warm)]
#[case("finished", ServiceClass::Idle)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn track_state_maps_to_service_class(#[case] phase: &str, #[case] expected: ServiceClass) {
    let (mut queue, first, second, mut rig, dir) = pending_selection();
    queue.config.action_at_item_end = ActionAtItemEnd::None;
    let mut priorities = Vec::new();
    let mut sources = Vec::new();
    hang_tick!();
    let first_lane = open(&mut queue, &mut rig, &dir, &mut priorities, &mut sources).await;
    hang_reset!();
    deck(&mut queue, &mut rig, &mut priorities);
    let lane = match phase {
        "playing" => {
            assert!(matches!(
                queue
                    .current_track()
                    .expect("playing track")
                    .snapshot()
                    .status,
                PlayingStatus::Playing { .. }
            ));
            first_lane
        }
        "fading_in" | "fading_out" => {
            select_successor(&mut queue, &mut rig, second);
            let second_lane = open(&mut queue, &mut rig, &dir, &mut priorities, &mut sources).await;
            let fades = deck(&mut queue, &mut rig, &mut priorities);
            let (item, dir, lane) = if phase == "fading_in" {
                (second, FadeDir::In, second_lane)
            } else {
                (first, FadeDir::Out, first_lane)
            };
            let active = queue
                .active
                .iter()
                .find(|active| active.item == item)
                .expect("fading track");
            assert!(fades.contains(&(active.slot, dir)));
            assert!(matches!(
                active.track.snapshot().status,
                PlayingStatus::Playing { .. }
            ));
            lane
        }
        "preloading" => {
            priorities.clear();
            with_outbox(&mut queue, &mut rig, |queue, out| {
                queue.load_track(
                    second,
                    Role::Preloaded,
                    Some(&mock::output(None).get()),
                    out,
                )
            })
            .expect("actual preload");
            hang_tick!();
            let lane = open(&mut queue, &mut rig, &dir, &mut priorities, &mut sources).await;
            hang_reset!();
            deck(&mut queue, &mut rig, &mut priorities);
            let active = queue
                .active
                .iter()
                .find(|active| active.item == second)
                .expect("preloaded track");
            assert_eq!(active.role, Role::Preloaded);
            assert!(matches!(
                active.track.snapshot().status,
                PlayingStatus::Loaded
            ));
            lane
        }
        "finished" => {
            priorities.clear();
            assert_eq!(queue.snapshot().current, Some(first));
            let slot = queue
                .current_track()
                .expect("finishing track")
                .snapshot()
                .slot
                .expect("finishing track holds a slot");
            with_outbox(&mut queue, &mut rig, |queue, out| {
                Player::settle(
                    queue,
                    TrackReceipt::Event(DeckEvent::Ended {
                        slot,
                        at: SessionFrame::new(512),
                    }),
                    out,
                )
            });
            let finished = queue
                .active
                .iter()
                .find(|active| active.item == first)
                .expect("finished track");
            assert_eq!(finished.slot, slot);
            assert!(matches!(
                finished.track.snapshot().status,
                PlayingStatus::Ended { .. }
            ));
            assert_eq!(queue.snapshot().current, Some(first));
            dispatcher(&mut queue, &mut rig, None, &mut priorities);
            first_lane
        }
        _ => panic!("one of the five original lifecycle phases"),
    };
    let class = priorities
        .iter()
        .rev()
        .find_map(|(actual_lane, class)| (*actual_lane == lane).then_some(*class));
    assert_eq!(
        class,
        Some(expected),
        "actual {phase} lifecycle must prioritize its own lane"
    );
}
