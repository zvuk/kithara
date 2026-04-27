//! Integration tests for Decoder seek operations.
//!
//! Tests `Decoder<Stream<File>>` seek forward/backward with real audio.

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ChunkOutcome, PcmReader},
    events::{AudioEvent, Event, EventBus},
    file::{File, FileConfig},
    stream::Stream,
};
use kithara_platform::time::{Duration, Instant, sleep};
use kithara_test_utils::{TestServerHelper, TestTempDir, temp_dir};

#[kithara::fixture]
async fn server() -> TestServerHelper {
    TestServerHelper::new().await
}

/// Open a remote test.mp3 as `Audio<Stream<File>>` with optional hw/sw backend
/// and optional event bus. Centralises the setup shared by every seek test.
async fn open_test_mp3(
    server: &TestServerHelper,
    temp_dir: &TestTempDir,
    prefer_hardware: bool,
    events: Option<EventBus>,
) -> Audio<Stream<File>> {
    let url = server.asset("test.mp3");
    let file_config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let mut config = AudioConfig::<File>::new(file_config)
        .with_hint("mp3")
        .with_prefer_hardware(prefer_hardware);
    if let Some(bus) = events {
        config = config.with_events(bus);
    }
    Audio::<Stream<File>>::new(config).await.unwrap()
}

async fn next_chunk(audio: &mut Audio<Stream<File>>, stage: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        match PcmReader::next_chunk(audio) {
            Ok(ChunkOutcome::Chunk(_)) => return,
            Ok(ChunkOutcome::Eof { .. }) => {
                panic!("unexpected EOF while waiting for {stage}");
            }
            Ok(ChunkOutcome::Pending { .. }) => {}
            Err(e) => panic!("decode error at {stage}: {e}"),
        }
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for decoded PCM at stage={stage}",
        );

        audio.preload().expect("preload must succeed");
        sleep(Duration::from_millis(10)).await;
    }
}

/// Create Decoder<Stream<File>> and verify spec.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn decoder_file_creates_successfully(
    #[future] server: TestServerHelper,
    temp_dir: TestTempDir,
) {
    let server = server.await;
    let decoder = open_test_mp3(&server, &temp_dir, false, None).await;

    let spec = decoder.spec();
    assert!(spec.sample_rate > 0);
    assert!(spec.channels > 0);
}

/// Decoder<Stream<File>> reads MP3 samples (no seek, just read).
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn decoder_file_reads_samples(#[future] server: TestServerHelper, temp_dir: TestTempDir) {
    let server = server.await;
    let mut decoder = open_test_mp3(&server, &temp_dir, false, None).await;

    next_chunk(&mut decoder, "initial read").await;
}

/// Decoder<Stream<File>> seeks to a single target and resumes decoding.
///
/// Covers three zero-warmup scenarios: seek to 0, seek forward (2s), seek
/// to 0 again. The backward-seek case (which needs a warmup prelude) is a
/// separate test.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::to_zero(Duration::from_secs(0))]
#[case::forward(Duration::from_secs(2))]
async fn decoder_file_single_seek(
    #[future] server: TestServerHelper,
    temp_dir: TestTempDir,
    #[case] target: Duration,
) {
    let server = server.await;
    let mut decoder = open_test_mp3(&server, &temp_dir, false, None).await;

    let spec = decoder.spec();
    assert!(spec.sample_rate > 0 && spec.channels > 0);

    next_chunk(&mut decoder, "before seek").await;

    decoder.seek(target).unwrap();

    next_chunk(&mut decoder, "after seek").await;

    let spec_after = decoder.spec();
    assert_eq!(spec.sample_rate, spec_after.sample_rate);
    assert_eq!(spec.channels, spec_after.channels);
}

/// Decoder<Stream<File>> can seek backward to the beginning after a warmup.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn decoder_file_seek_backward(#[future] server: TestServerHelper, temp_dir: TestTempDir) {
    let server = server.await;
    let mut decoder = open_test_mp3(&server, &temp_dir, false, None).await;

    for stage in 0..3 {
        next_chunk(&mut decoder, &format!("warmup chunk {stage}")).await;
    }

    decoder.seek(Duration::from_secs(3)).unwrap();
    next_chunk(&mut decoder, "after seek forward").await;

    decoder.seek(Duration::from_secs(0)).unwrap();
    next_chunk(&mut decoder, "after seek backward").await;
}

/// Decoder<Stream<File>> multiple seeks in sequence.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::sw(false)]
#[case::hw(true)]
async fn decoder_file_seek_multiple(
    #[future] server: TestServerHelper,
    temp_dir: TestTempDir,
    #[case] prefer_hardware: bool,
) {
    let server = server.await;
    let mut decoder = open_test_mp3(&server, &temp_dir, prefer_hardware, None).await;

    next_chunk(&mut decoder, "initial read").await;

    for target in [1, 5, 2, 0] {
        decoder.seek(Duration::from_secs(target)).unwrap();
        next_chunk(&mut decoder, &format!("after seek to {target}s")).await;
    }
}

/// Decoder<Stream<File>> events are emitted on seek.
#[kithara::test(
    tokio,
    browser,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn decoder_file_seek_emits_events(#[future] server: TestServerHelper, temp_dir: TestTempDir) {
    let server = server.await;
    let bus = EventBus::new(64);
    let mut events_rx = bus.subscribe();

    let mut decoder = open_test_mp3(&server, &temp_dir, false, Some(bus)).await;

    next_chunk(&mut decoder, "before seek events").await;
    decoder.seek(Duration::from_secs(2)).unwrap();

    let mut got_format = false;
    let mut got_seek = false;
    let mut buf = [0.0_f32; 1024];

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        while let Ok(ev) = events_rx.try_recv() {
            match ev {
                Event::Audio(AudioEvent::FormatDetected { .. }) => {
                    got_format = true;
                }
                Event::Audio(AudioEvent::SeekComplete { .. }) => {
                    got_seek = true;
                }
                _ => {}
            }
        }

        if got_format && got_seek {
            break;
        }

        assert!(
            Instant::now() <= deadline,
            "timed out waiting for FormatDetected/SeekComplete events",
        );

        use kithara::audio::ReadOutcome;
        match decoder.read(&mut buf) {
            Ok(ReadOutcome::Pending { .. }) => {
                decoder.preload().expect("preload must succeed");
                sleep(Duration::from_millis(10)).await;
            }
            Ok(ReadOutcome::Frames { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF while waiting for seek events");
            }
            Err(e) => panic!("decode error: {e}"),
        }
    }

    assert!(got_format, "should have received FormatDetected event");
    assert!(got_seek, "should have received SeekComplete event");
}
