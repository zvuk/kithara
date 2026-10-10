use std::num::{NonZeroU32, NonZeroUsize};

use kithara_command::Live;
use kithara_effects::LimiterConfig;
use kithara_platform::{sync::Arc, thread::sleep, time::Duration};
use kithara_test_utils::{
    bufpool::{TestPools, pools},
    kithara,
};
use kithara_worker::{Dispatcher, DispatcherConfig, TaskConfig, Worker, WorkerConfig};

use crate::{
    HostCore, HostSettings, PlayError,
    consts::{self, SESSION_PUMP_INTERVAL},
    rt::SessionOutput,
    session::{
        offline::{
            OfflineSessionClient, OfflineTaskHandle,
            task::{OfflineTaskConfig, spawn},
        },
        protocol::{HostDispatcher, ask},
        tests::{
            deck_probe::{Seen, next, probe, so_far, ticks},
            graph::empty_root,
        },
    },
};

type BaseCommand = crate::HostCommand<TestPools, dyn kithara_play::HostedDeck<TestPools>>;

/// An offline session with no graph that holds probe decks, and the
/// worker it runs on.
struct DeckSession {
    client: Arc<OfflineSessionClient<BaseCommand>>,
    _task: OfflineTaskHandle,
    _dispatcher: Dispatcher,
    _worker: Worker,
}

impl DeckSession {
    fn spawn() -> Self {
        let sample_rate = NonZeroU32::new(48_000).expect("test sample rate");
        let block = NonZeroU32::new(512).expect("test block");
        let (root, root_view) = empty_root(sample_rate);
        let worker = Worker::new(WorkerConfig::new());
        let dispatcher = worker.dispatcher(
            DispatcherConfig::builder()
                .name(consts::DECK_SESSION)
                .capacity(NonZeroUsize::MIN)
                .build(),
        );
        let (client, task) = spawn(
            &dispatcher,
            TaskConfig::new(),
            root,
            root_view,
            OfflineTaskConfig::builder()
                .declared_latency(Duration::ZERO)
                .output(SessionOutput::new(LimiterConfig::default()))
                .settings(
                    Live::new(HostSettings::builder().sample_rate(sample_rate).build())
                        .expect("the fixture settings are valid"),
                )
                .declick_frames(block)
                .channel_config(crate::HostConfig::offline(pools()).build().channel_config())
                .max_block_frames(block)
                .pools(pools())
                .build(),
            |owner: HostCore<TestPools>| owner,
        )
        .expect("the offline session starts");
        Self {
            client,
            _task: task,
            _dispatcher: dispatcher,
            _worker: worker,
        }
    }

    /// Returns once the session ran everything sent to it before.
    fn settle(&self) {
        self.client.position().expect("the session answers");
    }
}

impl Drop for DeckSession {
    fn drop(&mut self) {
        self.client.shutdown();
    }
}

#[kithara::test]
fn an_offline_session_ticks_its_decks_only_ahead_of_a_block() {
    let session = DeckSession::spawn();
    let (id, deck, seen) = probe();

    ask(&*session.client, crate::HostCommand::Register { id, deck })
        .map_err(PlayError::from)
        .expect("the session takes the deck");
    sleep(SESSION_PUMP_INTERVAL * 3);
    session.settle();
    assert_eq!(ticks(&so_far(&seen)), 0, "no clock ticks an offline deck");

    session
        .client
        .render(0, 512)
        .expect("render the first block");
    session.settle();
    let ticked = so_far(&seen);
    assert_eq!(ticks(&ticked), 1, "one tick ahead of the block");
    assert!(
        ticked.iter().any(
            |seen| matches!(seen, Seen::Ticked(thread) if thread.as_deref() == Some(consts::DECK_SESSION))
        ),
        "the session task ticks its decks"
    );
}

#[kithara::test]
fn an_offline_deck_drains_on_its_wake_with_no_block_rendered() {
    let session = DeckSession::spawn();
    let (id, deck, seen) = probe();
    ask(&*session.client, crate::HostCommand::Register { id, deck })
        .map_err(PlayError::from)
        .expect("the session takes the deck");
    let Seen::Held(waker) = next(&seen) else {
        panic!("the session holds the deck before anything else");
    };
    assert!(
        matches!(next(&seen), Seen::Drained(_)),
        "drained as it is held"
    );

    waker.wake_by_ref();

    assert!(
        matches!(next(&seen), Seen::Drained(thread) if thread.as_deref() == Some(consts::DECK_SESSION)),
        "a woken deck runs its commands on the session task before any tick"
    );
}
