#![cfg(not(target_arch = "wasm32"))]

//! `Queue::play` loads the current item through the player, which
//! starts the audio engine first — 130-400 ms against a real output device.
//! A track load completing inside that window fills the queue slot and is
//! picked up by the same call, so the consumption has to be read back from
//! the player rather than inferred from a status snapshot taken before it.
//! Get that wrong and the track stays `Loaded` over an emptied slot, which
//! turns every later select of it into `PlayError::ItemConsumed` — the
//! rejection the iOS switch storm reports.
//!
//! The engine-start window is a session gate here, so the interleaving is a
//! rendezvous rather than a timing window.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::ConsumerWakeMode,
    events::{Event, QueueEvent},
    platform::{
        sync::{Arc, Mutex, mpsc},
        time::Duration,
        tokio,
    },
    play::{
        AllocatedSlot, Cmd, NodeInputs, PlayError, PlayerConfig, PlayerImpl, Reply, ResourceConfig,
        ResourceSrc, SessionBinding, SessionDispatcher, SessionDuckingMode, SessionSampleRate,
        SharedEq, SlotId, bridge::slot_channels, player::PlayerControlSource,
    },
    queue::{Queue, QueueConfig, TrackSource, Transition},
};
use kithara_integration_tests::{
    TestTempDir,
    bufpool_ext::{TestPools, pools},
    kithara,
    offline::QueueTicker,
    temp_dir,
    test_defaults::Consts as Shared,
    waits::wait_for_event,
};
use kithara_test_fixtures::fixtures::tone_mp3;

const TRACK_COUNT: usize = 2;

/// Holds the first `StartPlayer` until the test releases it, standing in for
/// the audio-device stream start that makes the window wide on a real device.
struct StartGatedSession {
    gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
    next_player: AtomicU64,
    next_slot: AtomicU64,
    nodes: Mutex<Vec<NodeInputs>>,
}

impl StartGatedSession {
    fn new(entered: mpsc::Sender<()>, release: mpsc::Receiver<()>) -> Self {
        Self {
            gate: Mutex::new(Some((entered, release))),
            next_player: AtomicU64::new(1),
            next_slot: AtomicU64::new(0),
            nodes: Mutex::default(),
        }
    }
}

impl SessionDispatcher<TestPools> for StartGatedSession {
    fn exec(&self, cmd: Cmd<TestPools>) -> Result<Reply, PlayError> {
        let reply = match cmd {
            Cmd::StartPlayer { .. } => {
                if let Some((entered, release)) = self.gate.lock().take() {
                    entered.send(()).expect("test holds the entered receiver");
                    release.recv().expect("test holds the release sender");
                }
                Reply::Ok
            }
            Cmd::RegisterPlayer { .. } => {
                Reply::PlayerRegistered(self.next_player.fetch_add(1, Ordering::Relaxed))
            }
            Cmd::AllocateSlot { .. } => {
                let slot = SlotId::new(self.next_slot.fetch_add(1, Ordering::Relaxed));
                let (inputs, control) = slot_channels(SharedEq::new(10));
                self.nodes.lock().push(inputs);
                Reply::SlotAllocated(AllocatedSlot::new(control, slot))
            }
            Cmd::QuerySampleRate => Reply::SampleRate(SessionSampleRate::new(
                None,
                Shared::NON_ZERO_SAMPLE_RATE.get(),
            )),
            Cmd::SessionDucking => Reply::SessionDucking(SessionDuckingMode::Off),
            _ => Reply::Ok,
        };
        Ok(reply)
    }

    fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        ConsumerWakeMode::RealtimeDeferred
    }
}

fn spawn_ticker(queue: &Queue<TestPools>) -> QueueTicker {
    QueueTicker::spawn(queue.control(), Duration::from_millis(20))
}

/// A local fixture per track: the load has to run and land asynchronously,
/// but nothing about this test depends on how long it takes — the gate owns
/// the ordering — so it stays off the shared test server.
#[kithara::fixture]
fn gated_paths(temp_dir: TestTempDir, tone_mp3: &'static [u8]) -> (TestTempDir, Vec<PathBuf>) {
    let paths = (0..TRACK_COUNT)
        .map(|index| {
            let path = temp_dir.path().join(format!("gated-{index}.mp3"));
            fs::write(&path, tone_mp3).expect("fixture must be writable");
            path
        })
        .collect();
    (temp_dir, paths)
}

fn resource_config(path: &Path, store: &AssetStore<TestPools>) -> ResourceConfig<TestPools> {
    ResourceConfig::for_src(
        ResourceSrc::parse(path.to_string_lossy()).expect("absolute fixture path"),
    )
    .store(store.clone())
    .build()
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(180)))]
async fn a_track_play_consumed_mid_load_can_be_selected_again(
    gated_paths: (TestTempDir, Vec<PathBuf>),
) {
    let (temp_dir, paths) = gated_paths;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let session = Arc::new(StartGatedSession::new(entered_tx, release_rx));
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().into(),
        })
        .build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(Shared::NON_ZERO_SAMPLE_RATE)
            .worker(kithara::play::PlayWorker::new(
                kithara::play::PlayWorkerConfig::builder(pools).build(),
            ))
            .session(SessionBinding::new(session, Shared::NON_ZERO_SAMPLE_RATE))
            .build(),
    );
    let player_control = player.control();
    let queue = Arc::new(Queue::new(
        QueueConfig::builder()
            .player(player)
            .store(store.clone())
            .build(),
    ));
    let mut ticker = spawn_ticker(&queue);
    let mut status_rx = queue.subscribe();

    let ids: Vec<_> = (0..TRACK_COUNT)
        .map(|index| {
            queue.append(TrackSource::Config(Box::new(resource_config(
                &paths[index],
                &store,
            ))))
        })
        .collect::<Result<Vec<_>, _>>()
        .expect("queue is open while fixtures are appended");

    // `play` is issued while every track is still loading, exactly as the iOS
    // surface does, and parks inside the engine start.
    let playing = tokio::task::spawn_blocking({
        let queue = Arc::clone(&queue);
        move || queue.play()
    });
    tokio::task::spawn_blocking(move || entered_rx.recv())
        .await
        .expect("gate task must join")
        .expect("play must reach the engine start");

    // The first track's load lands inside the window the gate is holding open.
    wait_for_event(
        &mut status_rx,
        "the first track's load landing while play is inside the engine start",
        |event| {
            matches!(
                event,
                Event::Queue(QueueEvent::NextTrackReady { id, .. }) if *id == ids[0]
            )
        },
        Duration::from_secs(60),
    )
    .await
    .unwrap_or_else(|error| panic!("precondition: {error}"));

    release_tx.send(()).expect("gate is still parked");
    playing.await.expect("play must join");

    assert!(
        !player_control.item_has_resource(0),
        "precondition: play did not consume the load that landed inside the engine \
         start, so the reported window was never opened"
    );

    queue
        .select(ids[1], Transition::None)
        .expect("selecting the second track must be accepted");
    wait_for_event(
        &mut status_rx,
        "the second track becoming current",
        |event| {
            matches!(
                event,
                Event::Queue(QueueEvent::CurrentTrackChanged { id: Some(id) }) if *id == ids[1]
            )
        },
        Duration::from_secs(60),
    )
    .await
    .unwrap_or_else(|error| panic!("precondition: {error}"));

    queue
        .select(ids[0], Transition::None)
        .unwrap_or_else(|error| {
            panic!(
                "switching back to the track `play` consumed was rejected: {error} — the \
             queue still reports it as holding a resource the player no longer has"
            )
        });

    queue.clear();
    ticker.stop().await;
}
