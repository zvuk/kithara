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

use std::{fs, path::PathBuf};

use kithara::{
    events::{Event, QueueEvent},
    platform::{
        sync::{Arc, Mutex, mpsc},
        time::{self, Duration},
        tokio,
    },
    play::{Cmd, PlayError, PlayerConfig, PlayerImpl, Reply, ResourceConfig, SessionDispatcher},
    queue::{Queue, QueueConfig, TrackSource, Transition},
};
use kithara_integration_tests::{
    TestTempDir, audio_fixture::EmbeddedAudio, kithara, offline::OfflineSession, temp_dir,
    waits::wait_for_event,
};

const TRACK_COUNT: usize = 2;

/// Holds the first `StartPlayer` until the test releases it, standing in for
/// the audio-device stream start that makes the window wide on a real device.
struct StartGatedSession {
    inner: Arc<dyn SessionDispatcher>,
    gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}

impl StartGatedSession {
    fn new(
        inner: Arc<dyn SessionDispatcher>,
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    ) -> Self {
        Self {
            inner,
            gate: Mutex::new(Some((entered, release))),
        }
    }
}

impl SessionDispatcher for StartGatedSession {
    fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        if matches!(cmd, Cmd::StartPlayer { .. })
            && let Some((entered, release)) = self.gate.lock().take()
        {
            entered.send(()).expect("test holds the entered receiver");
            release.recv().expect("test holds the release sender");
        }
        self.inner.exec(cmd)
    }
}

fn spawn_ticker(queue: Arc<Queue>) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        loop {
            time::sleep(Duration::from_millis(20)).await;
            if queue.tick().is_err() {
                break;
            }
        }
    })
}

/// A local fixture per track: the load has to run and land asynchronously,
/// but nothing about this test depends on how long it takes — the gate owns
/// the ordering — so it stays off the shared test server.
fn fixture_path(temp_dir: &TestTempDir, index: usize) -> PathBuf {
    let path = temp_dir.path().join(format!("gated-{index}.mp3"));
    fs::write(&path, EmbeddedAudio::TEST_MP3_BYTES).expect("fixture must be writable");
    path
}

fn resource_config(temp_dir: &TestTempDir, index: usize) -> ResourceConfig {
    ResourceConfig::for_src(fixture_path(temp_dir, index).to_string_lossy())
        .expect("absolute fixture path")
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .store(kithara_integration_tests::disk_asset_store(temp_dir.path()))
        .build()
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(180)))]
async fn a_track_play_consumed_mid_load_can_be_selected_again(temp_dir: TestTempDir) {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let session = Arc::new(StartGatedSession::new(
        OfflineSession::arc_auto(),
        entered_tx,
        release_rx,
    ));
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .session(session)
            .build(),
    ));
    let queue = Arc::new(Queue::new(
        QueueConfig::default().with_player(Arc::clone(&player)),
    ));
    let ticker = spawn_ticker(Arc::clone(&queue));
    let mut status_rx = queue.subscribe();

    let ids: Vec<_> = (0..TRACK_COUNT)
        .map(|index| {
            queue.append(TrackSource::Config(Box::new(resource_config(
                &temp_dir, index,
            ))))
        })
        .collect();

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
        !player.item_has_resource(0),
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
    ticker.abort();
    let _ = ticker.await;
}
