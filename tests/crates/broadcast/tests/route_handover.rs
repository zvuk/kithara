use std::{
    env,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara::{
    assets::{AssetResource, AssetSource, AssetStore, ReadSide, ResourceKey},
    broadcast::{Broadcast, BroadcastConfig},
    encode::EncodeConfig,
    events::TrackId,
    net::{HttpClient, NetOptions},
    output::OutputGroup,
    platform::{
        CancelScope,
        sync::{Arc, Mutex},
        thread,
        time::{Duration, Instant},
    },
    play::Resource,
    record::{
        LiveRecorder, LiveRecordingConfig, LiveRecordingHandle, LiveRecordingReport,
        PartSinkFactory, RecordingConfig,
    },
    signal::AudioSpec,
    worker::{Worker, WorkerConfig},
};
use kithara_app::recording::{AssetPartSink, AssetPartSinkError};
use kithara_integration_tests::{
    audio_mock::TestPcmReader,
    bufpool_ext::{TestPools, pools},
    memory_asset_store,
    offline::{OfflinePlayerHarness, OfflinePlayerOptions, resource_from_reader},
};
use kithara_test_fixtures::integration_fixtures::broadcast_tone;
use url::Url;

use super::origin::Playlist;

const CHANNELS: u16 = 2;
const OLD_RATE: u32 = 44_100;
const NEW_RATE: u32 = 48_000;
const BLOCK_FRAMES: usize = 512;
const BLOCKS_PER_RATE: usize = 64;

struct RouteRecording;

#[derive(Clone)]
struct AssetFactory {
    keys: Arc<Mutex<Vec<ResourceKey>>>,
    source: AssetSource,
    store: AssetStore<TestPools>,
}

impl PartSinkFactory for AssetFactory {
    type Sink = AssetPartSink<TestPools>;

    fn open(&mut self, part: u64) -> Result<Self::Sink, AssetPartSinkError> {
        let key = self
            .store
            .scope::<RouteRecording>(&self.source)?
            .key(&AssetResource::Named {
                namespace: "route-handover".to_owned(),
                name: format!("master-{part}.wav"),
            })?;
        self.keys.lock().push(key.clone());
        AssetPartSink::acquire(&self.store, &key)
    }
}

fn tone_resource(broadcast_tone: Vec<f32>) -> Resource {
    let spec = AudioSpec::new(
        CHANNELS,
        NonZeroU32::new(OLD_RATE).expect("test rate is non-zero"),
    );
    resource_from_reader(TestPcmReader::from_samples(spec, broadcast_tone))
}

async fn playing_harness(broadcast_tone: Vec<f32>) -> OfflinePlayerHarness {
    let harness =
        OfflinePlayerHarness::with_sample_rate(OfflinePlayerOptions::builder().build(), OLD_RATE)
            .await;
    harness
        .with_player(move |player| {
            player.insert(tone_resource(broadcast_tone), TrackId::allocate(), None);
            player.select_item(0, true).expect("select tone");
        })
        .await;
    let _ = harness.render(BLOCK_FRAMES).await;
    let _ = harness.tick_and_drain().await;
    harness
}

async fn render_blocks(harness: &OfflinePlayerHarness) -> Vec<f32> {
    let mut rendered = Vec::with_capacity(BLOCKS_PER_RATE * BLOCK_FRAMES * usize::from(CHANNELS));
    for _ in 0..BLOCKS_PER_RATE {
        rendered.extend_from_slice(&harness.render(BLOCK_FRAMES).await);
        let _ = harness.tick_and_drain().await;
    }
    rendered
}

fn wait_recording(handle: &LiveRecordingHandle) -> LiveRecordingReport {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(result) = handle.finish() {
            return result.expect("route recording finishes");
        }
        assert!(Instant::now() < deadline, "route recording did not finish");
        thread::yield_now();
    }
}

fn wav_rate(store: &AssetStore<TestPools>, key: &ResourceKey) -> u32 {
    let reader = store
        .open_resource(key, None)
        .expect("open committed route recording");
    let mut header = [0_u8; 44];
    assert_eq!(reader.read_at(0, &mut header).expect("read WAV header"), 44);
    assert_eq!(&header[0..4], b"RIFF");
    assert_eq!(&header[8..12], b"WAVE");
    assert!(reader.len().is_some_and(|len| len > 44));
    u32::from_le_bytes(header[24..28].try_into().expect("WAV sample rate"))
}

#[kithara::test(tokio, flash(false), timeout(Duration::from_secs(60)))]
async fn route_change_continues_recording_and_broadcast_in_new_segments(broadcast_tone: Vec<f32>) {
    let harness = playing_harness(broadcast_tone).await;
    let pools = pools();
    let worker = Worker::new(WorkerConfig::new());
    let store = memory_asset_store();
    let keys = Arc::new(Mutex::new(Vec::new()));
    let recorder_config = LiveRecordingConfig::builder(
        worker.clone(),
        pools.clone(),
        AssetFactory {
            keys: Arc::clone(&keys),
            source: AssetSource::Local {
                path: env::temp_dir().join("kithara-route-handover"),
            },
            store: store.clone(),
        },
    )
    .recording(
        RecordingConfig::builder()
            .encode(
                EncodeConfig::builder()
                    .sample_rate(OLD_RATE)
                    .channels(CHANNELS)
                    .build(),
            )
            .build(),
    )
    .buffer_frames(NonZeroUsize::new(131_072).expect("recorder buffer"))
    .tick_frames(NonZeroUsize::new(4_096).expect("recorder tick"))
    .build();
    let (recording_output, recording_handle) =
        LiveRecorder::start(recorder_config).expect("start recorder");
    let scope = CancelScope::new(None);
    let broadcast_config = BroadcastConfig::builder(worker, pools.clone())
        .cancel(scope.token())
        .sample_rate(OLD_RATE)
        .channels(CHANNELS)
        .segment_target(Duration::from_millis(500))
        .buffer_frames(NonZeroUsize::new(131_072).expect("broadcast buffer"))
        .tick_frames(NonZeroUsize::new(4_096).expect("broadcast tick"))
        .build();
    let (broadcast_output, broadcast_handle) =
        Broadcast::start(broadcast_config).expect("start broadcast");
    let url = broadcast_handle.url().to_owned();
    let base = Url::parse(&url).expect("broadcast URL");
    let client = HttpClient::new(NetOptions::default(), pools, scope.token());
    let mut outputs = OutputGroup::new();
    outputs.push(recording_output);
    outputs.push(broadcast_output);
    harness
        .host()
        .enable_outputs(outputs)
        .await
        .expect("enable recorder and broadcast");

    let before = render_blocks(&harness).await;
    harness
        .host()
        .restart_stream(NEW_RATE)
        .await
        .expect("restart at the new device rate");
    let after = render_blocks(&harness).await;

    assert_eq!(broadcast_handle.url(), url);
    assert!(before.iter().any(|sample| sample.abs() > 0.0));
    assert!(after.iter().any(|sample| sample.abs() > 0.0));
    harness
        .host()
        .disable_mix_tap()
        .await
        .expect("release output group");
    let report = wait_recording(&recording_handle);
    broadcast_handle.stop();

    assert_eq!(
        report.frames,
        u64::try_from(2 * BLOCKS_PER_RATE * BLOCK_FRAMES).expect("rendered frames fit")
    );
    assert_eq!(report.parts, 2);
    let committed = keys.lock().clone();
    assert_eq!(committed.len(), 2);
    assert_eq!(wav_rate(&store, &committed[0]), OLD_RATE);
    assert_eq!(wav_rate(&store, &committed[1]), NEW_RATE);
    assert_eq!(broadcast_handle.status().dropped_samples, 0);

    let playlist_url = base.join("v/0/live.m3u8").expect("playlist URL");
    let playlist = client
        .get_bytes(playlist_url, None)
        .await
        .expect("fetch route-change playlist");
    let playlist =
        Playlist::parse(String::from_utf8(playlist.to_vec()).expect("playlist is UTF-8"));
    assert!(playlist.text.contains("#EXT-X-DISCONTINUITY\n"));
    assert!(
        playlist
            .sequences()
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    );
    assert!(playlist.text.contains("#EXT-X-ENDLIST\n"));
    harness.close().await;
}
