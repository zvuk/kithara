//! Integration tests for Decoder seek operations.
//!
//! Tests `Decoder<Stream<File>>` seek forward/backward with real audio.

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, PcmReader},
    events::{AudioEvent, EventBus},
    file::{File, FileConfig},
    stream::Stream,
};
use kithara_integration_tests::audio_fixture::AudioTestServer;
use kithara_platform::time::{Duration, Instant, sleep};
use kithara_test_utils::{TestTempDir, temp_dir};

#[kithara::fixture]
async fn server() -> AudioTestServer {
    AudioTestServer::new().await
}

async fn next_chunk(audio: &mut Audio<Stream<File>>, stage: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        if PcmReader::next_chunk(audio).is_some() {
            return;
        }

        assert!(!audio.is_eof(), "unexpected EOF while waiting for {stage}");
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for decoded PCM at stage={stage}",
        );

        audio.preload();
        sleep(Duration::from_millis(10)).await;
    }
}

/// Create Decoder<Stream<File>> and verify spec.
#[kithara::test(tokio, browser, timeout(Duration::from_secs(30)))]
async fn decoder_file_creates_successfully(
    #[future] server: AudioTestServer,
    temp_dir: TestTempDir,
) {
    let server = server.await;
    let url = server.mp3_url();

    let file_config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");

    let decoder = Audio::<Stream<File>>::new(config).await.unwrap();

    let spec = decoder.spec();
    assert!(spec.sample_rate > 0);
    assert!(spec.channels > 0);
}

/// Decoder<Stream<File>> reads MP3 samples (no seek, just read).
#[kithara::test(tokio, browser, timeout(Duration::from_secs(30)))]
async fn decoder_file_reads_samples(#[future] server: AudioTestServer, temp_dir: TestTempDir) {
    let server = server.await;
    let url = server.mp3_url();

    let file_config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
    let mut decoder = Audio::<Stream<File>>::new(config).await.unwrap();

    next_chunk(&mut decoder, "initial read").await;
}

/// Seek to 0: read, seek to beginning, read again.
#[kithara::test(tokio, browser, timeout(Duration::from_secs(30)))]
async fn decoder_file_seek_to_zero(#[future] server: AudioTestServer, temp_dir: TestTempDir) {
    let server = server.await;
    let url = server.mp3_url();

    let file_config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
    let mut decoder = Audio::<Stream<File>>::new(config).await.unwrap();

    next_chunk(&mut decoder, "before seek to zero").await;
    decoder.seek(Duration::from_secs(0)).unwrap();
    next_chunk(&mut decoder, "after seek to zero").await;
}

/// Decoder<Stream<File>> reads MP3 samples and can seek forward.
#[kithara::test(tokio, browser, timeout(Duration::from_secs(30)))]
async fn decoder_file_seek_forward(#[future] server: AudioTestServer, temp_dir: TestTempDir) {
    let server = server.await;
    let url = server.mp3_url();

    let file_config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
    let mut decoder = Audio::<Stream<File>>::new(config).await.unwrap();

    let spec = decoder.spec();
    assert!(spec.sample_rate > 0);
    assert!(spec.channels > 0);

    next_chunk(&mut decoder, "before seek forward").await;
    assert!(!decoder.is_eof());

    decoder.seek(Duration::from_secs(2)).unwrap();
    assert!(!decoder.is_eof(), "should not be EOF after seek forward");

    next_chunk(&mut decoder, "after seek forward").await;

    let spec_after = decoder.spec();
    assert_eq!(spec.sample_rate, spec_after.sample_rate);
    assert_eq!(spec.channels, spec_after.channels);
}

/// Decoder<Stream<File>> can seek backward to the beginning.
#[kithara::test(tokio, browser, timeout(Duration::from_secs(30)))]
async fn decoder_file_seek_backward(#[future] server: AudioTestServer, temp_dir: TestTempDir) {
    let server = server.await;
    let url = server.mp3_url();

    let file_config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
    let mut decoder = Audio::<Stream<File>>::new(config).await.unwrap();

    for stage in 0..3 {
        next_chunk(&mut decoder, &format!("warmup chunk {stage}")).await;
    }

    decoder.seek(Duration::from_secs(3)).unwrap();
    next_chunk(&mut decoder, "after seek forward").await;

    decoder.seek(Duration::from_secs(0)).unwrap();
    assert!(!decoder.is_eof(), "should not be EOF after seek to start");
    next_chunk(&mut decoder, "after seek backward").await;
}

/// Decoder<Stream<File>> multiple seeks in sequence.
#[kithara::test(tokio, browser, timeout(Duration::from_secs(30)))]
async fn decoder_file_seek_multiple(#[future] server: AudioTestServer, temp_dir: TestTempDir) {
    let server = server.await;
    let url = server.mp3_url();

    let file_config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
    let mut decoder = Audio::<Stream<File>>::new(config).await.unwrap();

    next_chunk(&mut decoder, "initial read").await;

    decoder.seek(Duration::from_secs(1)).unwrap();
    next_chunk(&mut decoder, "after seek to 1s").await;

    decoder.seek(Duration::from_secs(5)).unwrap();
    next_chunk(&mut decoder, "after seek to 5s").await;

    decoder.seek(Duration::from_secs(2)).unwrap();
    next_chunk(&mut decoder, "after seek back to 2s").await;

    decoder.seek(Duration::from_secs(0)).unwrap();
    next_chunk(&mut decoder, "after seek to 0s").await;
}

/// Decoder<Stream<File>> events are emitted on seek.
#[kithara::test(tokio, browser, timeout(Duration::from_secs(30)))]
async fn decoder_file_seek_emits_events(#[future] server: AudioTestServer, temp_dir: TestTempDir) {
    let server = server.await;
    let url = server.mp3_url();

    let bus = EventBus::new(64);
    let mut events_rx = bus.subscribe();

    let file_config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let config = AudioConfig::<File>::new(file_config)
        .with_hint("mp3")
        .with_events(bus);
    let mut decoder = Audio::<Stream<File>>::new(config).await.unwrap();

    next_chunk(&mut decoder, "before seek events").await;
    decoder.seek(Duration::from_secs(2)).unwrap();

    let mut got_format = false;
    let mut got_seek = false;
    let mut buf = [0.0_f32; 1024];

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        while let Ok(ev) = events_rx.try_recv() {
            match ev {
                kithara::events::Event::Audio(AudioEvent::FormatDetected { .. }) => {
                    got_format = true;
                }
                kithara::events::Event::Audio(AudioEvent::SeekComplete { .. }) => {
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

        let read = decoder.read(&mut buf);
        if read == 0 {
            assert!(
                !decoder.is_eof(),
                "unexpected EOF while waiting for seek events",
            );
            decoder.preload();
            sleep(Duration::from_millis(10)).await;
        }
    }

    assert!(got_format, "should have received FormatDetected event");
    assert!(got_seek, "should have received SeekComplete event");
}
