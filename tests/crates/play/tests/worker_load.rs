#![cfg(not(target_arch = "wasm32"))]

use std::{future::poll_fn, num::NonZeroUsize, task::Poll};

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::AudioObserverSlot,
    platform::time::Duration,
    play::{
        DispatcherProtocol, LoadRefusal, PlayWorker, PlayWorkerConfig, ResourceConfig,
        ResourceLoad, ResourceSrc,
    },
    warp::{SpeedCurve, WarpConfig},
};
use kithara_command::{Batch, ChannelConfig, Outcome, Receipt, Rejection, Sender, When, channel};
use kithara_integration_tests::usdt_trace::{self, Scope};
use kithara_render::{Dispatched, DispatcherCommand, LaneStart, LoadRequest, ServiceClass};
use kithara_test_fixtures::fixtures::tone_mp3;
use kithara_test_utils::{TestTempDir, temp_dir};

use crate::bufpool_ext::{TestPools, pools};

fn opened(trace: &Scope) -> usize {
    trace.events_of("source_opened").len()
}

#[kithara::test(tokio, timeout(Duration::from_secs(30)))]
async fn a_load_opens_its_source_once_and_a_load_past_capacity_opens_nothing(
    temp_dir: TestTempDir,
) {
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools())
            .capacity(NonZeroUsize::MIN)
            .build(),
    );
    let trace = usdt_trace::scope();

    let (mut sender, inbox) = channel::<Loads>(ChannelConfig::builder().build());
    let _dispatcher = worker
        .start_dispatcher(inbox)
        .expect("the worker owns its dispatcher");
    sender
        .send(When::Next, load(&worker, &temp_dir, "a.mp3"))
        .expect("first load batch");
    let receipt = answered(&mut sender).await;
    let (
        Outcome::Applied {
            data: Dispatched::Loaded(held),
            ..
        },
        _,
    ) = receipt.into()
    else {
        panic!("the first load fits the worker");
    };
    assert_eq!(opened(&trace), 1, "a load opens its source once");

    sender
        .send(When::Next, load(&worker, &temp_dir, "b.mp3"))
        .expect("second load batch");
    let refused = answered(&mut sender).await;
    assert!(
        matches!(
            refused.outcome(),
            Outcome::Rejected(Rejection::Refused(LoadRefusal::Capacity { capacity: 1 }))
        ),
        "a load past capacity is refused for capacity"
    );
    assert_eq!(
        opened(&trace),
        1,
        "a load the worker cannot hold opens nothing"
    );
    sender
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![DispatcherCommand::Release(held.lane)],
            },
        )
        .expect("release lane batch");
    assert!(matches!(
        answered(&mut sender).await.outcome(),
        Outcome::Applied {
            data: Dispatched::Released,
            ..
        }
    ));
    sender
        .send(When::Next, load(&worker, &temp_dir, "c.mp3"))
        .expect("reload batch");
    let reloaded = answered(&mut sender).await;
    assert!(
        matches!(
            reloaded.outcome(),
            Outcome::Applied {
                data: Dispatched::Loaded(_),
                ..
            }
        ),
        "a released lane frees its slot"
    );
    assert_eq!(opened(&trace), 2, "the next load opens its own source");
}

type Loads = DispatcherProtocol<ResourceLoad<TestPools>>;

/// A one-load batch opening a local MP3 from its own file under `dir`.
fn load(worker: &PlayWorker<TestPools>, dir: &TestTempDir, name: &str) -> Batch<Loads> {
    let path = dir.write(name, tone_mp3());
    let store = AssetStore::builder(worker.pools().clone())
        .backend(StorageBackend::Disk {
            root: dir.path().join(format!("{name}.cache")),
        })
        .build();
    let warp = WarpConfig::builder().build();
    let start = LaneStart {
        speed: SpeedCurve::Constant(warp.speed()),
        keylock: warp.keylock(),
        backend: warp.backend(),
    };
    let config = ResourceConfig::for_src(ResourceSrc::Path(path))
        .store(store)
        .worker(worker.clone())
        .warp(warp)
        .build();
    Batch {
        basis: Vec::new(),
        commands: vec![DispatcherCommand::Load(Box::new(LoadRequest {
            class: ServiceClass::Warm,
            item: ResourceLoad::new(config, Box::new(AudioObserverSlot::default().relay())),
            position: Duration::ZERO,
            start,
            inbox: worker.lane_channel().1,
        }))],
    }
}

/// The next receipt from the dispatcher's configured worker owner.
async fn answered(sender: &mut Sender<Loads>) -> Receipt<Loads> {
    let receipt = poll_fn(|cx| {
        sender.hold(cx.waker().clone());
        sender.receipts().next().map_or(Poll::Pending, Poll::Ready)
    })
    .await;
    sender.release();
    receipt
}

#[kithara::test(tokio, timeout(Duration::from_secs(30)))]
async fn the_dispatcher_opens_each_load_once_and_answers_a_load_past_capacity_for_capacity(
    temp_dir: TestTempDir,
) {
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools())
            .capacity(NonZeroUsize::MIN)
            .build(),
    );
    let trace = usdt_trace::scope();
    let (mut sender, inbox) = channel::<Loads>(ChannelConfig::builder().build());
    let _dispatcher = worker
        .start_dispatcher(inbox)
        .expect("the worker owns its dispatcher");

    let first = sender
        .send(When::Next, load(&worker, &temp_dir, "a.mp3"))
        .expect("the channel has room");
    let receipt = answered(&mut sender).await;
    assert_eq!(receipt.seq(), first);
    let (
        Outcome::Applied {
            data: Dispatched::Loaded(held),
            ..
        },
        _,
    ) = receipt.into()
    else {
        panic!("the first load fits the worker");
    };
    assert_eq!(opened(&trace), 1, "a load opens its source once");

    let second = sender
        .send(When::Next, load(&worker, &temp_dir, "b.mp3"))
        .expect("the channel has room");
    let receipt = answered(&mut sender).await;
    assert_eq!(receipt.seq(), second);
    assert!(
        matches!(
            receipt.outcome(),
            Outcome::Rejected(Rejection::Refused(LoadRefusal::Capacity { capacity: 1 }))
        ),
        "a load past capacity is answered for capacity: {:?}",
        receipt.outcome()
    );
    assert_eq!(
        opened(&trace),
        1,
        "a load the worker cannot hold opens nothing"
    );
    sender
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![DispatcherCommand::SetPriority(
                    held.lane,
                    ServiceClass::Audible,
                )],
            },
        )
        .expect("prioritize resident lane");
    assert!(
        matches!(
            answered(&mut sender).await.outcome(),
            Outcome::Applied {
                data: Dispatched::Prioritized,
                ..
            }
        ),
        "the dispatcher runs while its sender lives"
    );
}

/// The worker refills every deck's ring, and a ring drains while its worker
/// waits for a CPU, so the worker asks the OS to run it ahead of ordinary work.
/// Whether the OS grants that is the machine's policy, not the worker's.
#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(30)))]
async fn the_play_worker_asks_the_os_to_schedule_it_as_an_audio_feed() {
    let trace = usdt_trace::scope();
    let _worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());

    trace
        .wait_for(|events| {
            events
                .iter()
                .any(|event| event.probe == "thread_class" && event.field("audio_feed") == Some(1))
        })
        .await;
    drop(trace);
}
