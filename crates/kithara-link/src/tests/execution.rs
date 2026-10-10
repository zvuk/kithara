use std::sync::Arc;

use kithara_assets::AssetStore;
use kithara_command::{Batch, Outcome, Receipt, Rejection, When};
use kithara_host::HostOwner;
use kithara_play::{
    PlayWorker, PlayWorkerConfig, Player, PlayerConfig, PlayerFactory, PlayerImpl, ResourceConfig,
    ResourceSrc, TrackCommand, TrackFactory, TrackReceipt, TrackSettings, TrackStatus,
    mock::{self, DeckRig},
};
use kithara_render::{
    bridge::{DeckProtocol, Slot},
    rt::DeckMixerConfig,
};
use kithara_signal::{AudioSpec, SessionFrame};
use kithara_test_utils::{
    TestTempDir,
    bufpool::{TestPools, pools},
    kithara,
};

use super::{
    fixtures::{answer, grid, load, loaded, position, resource, sounding, trajectory},
    host_fixture::{host, register},
};
use crate::{LinkConfig, Linked, SyncStatus};

fn settle(
    deck: &mut Linked<PlayerImpl<TestPools>>,
    rig: &mut DeckRig<TestPools>,
    receipt: Receipt<DeckProtocol>,
) {
    let seq = receipt.seq();
    let (outcome, mut batch) = receipt.into();
    rig.with_outbox(|out| {
        deck.settle(
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            out,
        )
    })
    .expect("live deck scope");
}

// Ruling: spec 3/5.5 removes staged sync lanes; supersession cancels queued starts by Seq on the one resident player.
#[kithara::test(tokio)]
async fn a_lane_dropped_before_its_turn_is_reported_only_by_its_cancellation() {
    let directory = TestTempDir::new();
    let path = directory.path().join("track.wav");
    let samples = vec![0.25; usize::try_from(mock::SAMPLE_RATE.get()).expect("sample rate") * 2];
    mock::write_pcm_wav(&path, &samples, AudioSpec::new(2, mock::SAMPLE_RATE)).expect("local WAV");
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
    let config: ResourceConfig<TestPools> = ResourceConfig::for_src(ResourceSrc::Path(path))
        .store(AssetStore::builder(pools()).build())
        .worker(worker.clone())
        .host_sample_rate(mock::SAMPLE_RATE)
        .build();
    let (_, source) = mock::resource_tracks(&config).expect("local source");
    let futures::future::Either::Left(source) = source else {
        panic!("WAV is a local source")
    };
    let fixture = mock::track_load(
        source,
        Arc::from("resident"),
        worker,
        mock::SAMPLE_RATE,
        |worker, config, position, start, inbox| {
            Box::pin(async move { worker.load(config, position, start, inbox).await })
        },
    );
    let track: PlayerImpl<TestPools> = PlayerFactory
        .track(PlayerConfig {
            item: kithara_events::TrackId::allocate(),
            slot: Some(Slot::new(0)),
            settings: TrackSettings::default(),
        })
        .expect("track");
    let mut deck = Linked::new(track, LinkConfig::default(), trajectory(120.0, 4));
    let mut rig = DeckRig::new(DeckMixerConfig::default()).expect("deck scope");
    rig.with_outbox(|out| {
        deck.apply(
            TrackCommand::Load {
                item: resource(),
                position: position(0),
            },
            out,
        )
    })
    .expect("scope")
    .expect("one load");
    let opened = rig
        .load_fixture(fixture, position(0))
        .await
        .expect("resident lane");
    let receipt = rig.open(Ok(opened)).expect("one dispatcher receipt");
    rig.with_outbox(|out| deck.settle(TrackReceipt::Loaded(receipt), out))
        .expect("scope");
    for receipt in rig
        .block(SessionFrame::new(0), 0.0)
        .expect("attachment block")
    {
        settle(&mut deck, &mut rig, receipt);
    }
    assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Loaded);
    let first = rig
        .with_outbox(|out| {
            deck.apply(
                TrackCommand::Play {
                    at: When::At(SessionFrame::new(24_000)),
                },
                out,
            )
        })
        .expect("scope")
        .expect("first entry")
        .expect("first sequence");
    let first_receipts = rig
        .block(SessionFrame::new(24_000), 0.0)
        .expect("first turn");
    assert_eq!(first_receipts.len(), 1);
    assert_eq!(first_receipts[0].seq(), first);
    assert!(
        matches!(first_receipts[0].outcome(), Outcome::Applied { at, .. } if *at == SessionFrame::new(24_000))
    );
    for receipt in first_receipts {
        settle(&mut deck, &mut rig, receipt);
    }
    let mut queued = Vec::new();
    for frame in [48_000, 72_000] {
        queued.push(
            rig.with_outbox(|out| {
                deck.apply(
                    TrackCommand::Play {
                        at: When::At(SessionFrame::new(frame)),
                    },
                    out,
                )
            })
            .expect("scope")
            .expect("queued entry")
            .expect("queued sequence"),
        );
    }
    rig.with_outbox(|out| deck.apply(TrackCommand::Supersede, out))
        .expect("scope")
        .expect("cancel queued entries");
    rig.with_outbox(|out| deck.apply(TrackCommand::Release, out))
        .expect("scope")
        .expect("release resident lane");
    let receipts = rig
        .block(SessionFrame::new(24_001), 0.0)
        .expect("cancellation turn");
    let cancelled: Vec<_> = receipts
        .iter()
        .filter(|receipt| queued.contains(&receipt.seq()))
        .collect();
    assert_eq!(cancelled.len(), 2);
    assert_eq!(
        cancelled
            .iter()
            .map(|receipt| receipt.seq())
            .collect::<Vec<_>>(),
        queued
    );
    assert!(
        cancelled
            .iter()
            .all(|receipt| matches!(receipt.outcome(), Outcome::Rejected(Rejection::Stale)))
    );
    for receipt in receipts {
        settle(&mut deck, &mut rig, receipt);
    }
    assert_eq!(deck.snapshot().as_ref().status, TrackStatus::Released);
    assert_eq!(deck.snapshot().sync, SyncStatus::Off);
    assert!(
        rig.block(SessionFrame::new(72_000), 0.0)
            .expect("obsolete turn")
            .is_empty()
    );
}

// Ruling: spec 4.5/4.6 replaces attached group topology with one item/load-owned track grid, never a nested group.
#[kithara::test]
fn an_attached_group_owns_its_track_geometry_as_its_only_member() {
    let (mut deck, control, mut rig, loading) = loaded(grid(24_000, 0, Some((4, 0)), 960_000));
    let item = deck.snapshot().as_ref().item;
    let slot = deck.snapshot().as_ref().slot;
    let sync = rig
        .run(|out| crate::LinkedPlayer::sync(&mut deck, true, out))
        .expect("sync")
        .expect("cue");
    assert_eq!(control.commands().len(), 2);
    assert_eq!(control.commands()[0].0, sync);
    assert_eq!(control.commands()[1].0, sync);
    rig.apply(&mut deck, sync, 0);
    assert_eq!(deck.snapshot().as_ref().item, item);
    assert_eq!(deck.snapshot().as_ref().slot, slot);
    control.clear();
    answer(
        &mut deck,
        &mut rig,
        loading,
        grid(24_000, 12_000, Some((4, 0)), 960_000),
    );
    assert_eq!(deck.snapshot().as_ref().item, item);
    assert_eq!(deck.snapshot().as_ref().slot, slot);
    assert!(
        !control
            .commands()
            .iter()
            .any(|(_, command)| matches!(command, super::fixtures::Command::Load(_)))
    );
    let replacement = load(&mut deck, &mut rig, 0);
    rig.apply(&mut deck, replacement, 0);
    control.clear();
    answer(
        &mut deck,
        &mut rig,
        loading,
        grid(24_000, 0, Some((4, 0)), 960_000),
    );
    assert_eq!(
        deck.snapshot().sync,
        SyncStatus::WaitingForGrid {
            required: position(0)
        }
    );
    assert!(control.commands().is_empty());
}

// Ruling: spec 4.6 removes recursive groups; a receipt routed by DeckId updates only the registered issuing player.
#[kithara::test]
fn a_receipt_reaches_the_nested_group_that_issued_it() {
    let (mut host, _) = host();
    let (deck, control, _, _) = sounding();
    control.edit(|script| script.snapshot.status = TrackStatus::Loaded);
    let (id, probe) = register(&mut host, deck, &control);
    let (sibling, sibling_control, _, _) = loaded(grid(24_000, 0, Some((4, 0)), 960_000));
    let (_, sibling_probe) = register(&mut host, sibling, &sibling_control);
    let sibling_before = sibling_probe.read(|observation| observation.snapshot.clone());
    probe.send(TrackCommand::Play {
        at: When::At(SessionFrame::new(96_000)),
    });
    host.with_deck(id, &mut |deck, out, pass| deck.drain(pass, out))
        .expect("issuing deck");
    let commands = control.commands();
    assert_eq!(commands.len(), 1);
    let seq = commands[0].0;
    assert_eq!(
        commands[0].1,
        super::fixtures::Command::Play(When::At(SessionFrame::new(96_000)))
    );
    let outcome = Outcome::Applied {
        at: SessionFrame::new(96_000),
        data: (),
    };
    let mut batch = Batch {
        basis: vec![(Slot::new(0), None)],
        commands: Vec::new(),
    };
    host.with_deck(id, &mut |deck, out, pass| {
        deck.settle(
            TrackReceipt::Deck {
                seq,
                outcome: &outcome,
                batch: &mut batch,
            },
            pass,
            out,
        );
    })
    .expect("routed receipt");
    probe.read(|observation| {
        assert_eq!(observation.snapshot.sync, SyncStatus::On);
        assert_eq!(
            observation.snapshot.as_ref().status,
            TrackStatus::Playing {
                since: SessionFrame::new(96_000)
            }
        );
    });
    sibling_probe.read(|observation| {
        assert_eq!(observation.snapshot.sync, SyncStatus::Off);
        assert_eq!(
            observation.snapshot.as_ref().item,
            sibling_before.as_ref().item
        );
        assert_eq!(
            observation.snapshot.as_ref().status,
            sibling_before.as_ref().status
        );
        assert_eq!(
            observation.snapshot.as_ref().position,
            sibling_before.as_ref().position
        );
    });
    assert!(sibling_control.commands().is_empty());
}
