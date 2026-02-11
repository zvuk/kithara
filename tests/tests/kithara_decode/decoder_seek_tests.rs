//! Integration tests for Decoder seek operations.
//!
//! Tests `Decoder<Stream<File>>` seek forward/backward with real audio.
//! All `decoder.read()`/`seek()` calls run in `spawn_blocking` because
//! the internal kanal channel uses blocking `recv()`.

use std::time::Duration;

use kithara_assets::StoreOptions;
use kithara_audio::{Audio, AudioConfig, AudioEvent, AudioPipelineEvent};
use kithara_file::{File, FileConfig};
use kithara_stream::Stream;
use rstest::{fixture, rstest};
use tempfile::TempDir;

use super::fixture::AudioTestServer;
use crate::common::fixtures::temp_dir;

#[fixture]
async fn server() -> AudioTestServer {
    AudioTestServer::new().await
}

/// Create Decoder<Stream<File>> and verify spec.
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decoder_file_creates_successfully(#[future] server: AudioTestServer, temp_dir: TempDir) {
    let server = server.await;
    let url = server.mp3_url();

    let file_config = FileConfig::new(url.into())
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_look_ahead_bytes(None);
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");

    let decoder = Audio::<Stream<File>>::new(config).await.unwrap();

    let spec = decoder.spec();
    assert!(spec.sample_rate > 0);
    assert!(spec.channels > 0);
}

/// Decoder<Stream<File>> reads MP3 samples (no seek, just read).
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decoder_file_reads_samples(#[future] server: AudioTestServer, temp_dir: TempDir) {
    let server = server.await;
    let url = server.mp3_url();

    let file_config = FileConfig::new(url.into())
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_look_ahead_bytes(None);
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
    let mut decoder = Audio::<Stream<File>>::new(config).await.unwrap();

    tokio::task::spawn_blocking(move || {
        let mut buf = [0.0f32; 1024];
        let n = decoder.read(&mut buf);
        assert!(n > 0, "should read samples");
    })
    .await
    .unwrap();
}

/// Seek to 0: read, seek to beginning, read again.
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decoder_file_seek_to_zero(#[future] server: AudioTestServer, temp_dir: TempDir) {
    let server = server.await;
    let url = server.mp3_url();

    let file_config = FileConfig::new(url.into())
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_look_ahead_bytes(None);
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
    let mut decoder = Audio::<Stream<File>>::new(config).await.unwrap();

    tokio::task::spawn_blocking(move || {
        let mut buf = [0.0f32; 1024];

        let n = decoder.read(&mut buf);
        assert!(n > 0);

        decoder.seek(Duration::from_secs(0)).unwrap();

        let n2 = decoder.read(&mut buf);
        assert!(n2 > 0, "should read after seek to 0");
    })
    .await
    .unwrap();
}

/// Decoder<Stream<File>> reads MP3 samples and can seek forward.
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decoder_file_seek_forward(#[future] server: AudioTestServer, temp_dir: TempDir) {
    let server = server.await;
    let url = server.mp3_url();

    let file_config = FileConfig::new(url.into())
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_look_ahead_bytes(None);
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
    let mut decoder = Audio::<Stream<File>>::new(config).await.unwrap();

    let spec = decoder.spec();
    assert!(spec.sample_rate > 0);
    assert!(spec.channels > 0);

    tokio::task::spawn_blocking(move || {
        // Read initial samples.
        let mut buf = [0.0f32; 1024];
        let n1 = decoder.read(&mut buf);
        assert!(n1 > 0, "should read initial samples");
        assert!(!decoder.is_eof());

        // Seek forward to 2 seconds.
        decoder.seek(Duration::from_secs(2)).unwrap();
        assert!(!decoder.is_eof(), "should not be EOF after seek forward");

        // Read samples after seek.
        let n2 = decoder.read(&mut buf);
        assert!(n2 > 0, "should read samples after seek forward");

        // Spec should remain the same.
        let spec_after = decoder.spec();
        assert_eq!(spec.sample_rate, spec_after.sample_rate);
        assert_eq!(spec.channels, spec_after.channels);
    })
    .await
    .unwrap();
}

/// Decoder<Stream<File>> can seek backward to the beginning.
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decoder_file_seek_backward(#[future] server: AudioTestServer, temp_dir: TempDir) {
    let server = server.await;
    let url = server.mp3_url();

    let file_config = FileConfig::new(url.into())
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_look_ahead_bytes(None);
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
    let mut decoder = Audio::<Stream<File>>::new(config).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Read some data to advance position.
        let mut buf = [0.0f32; 4096];
        let mut total = 0;
        for _ in 0..10 {
            let n = decoder.read(&mut buf);
            if n == 0 {
                break;
            }
            total += n;
        }
        assert!(total > 0, "should read some samples before seeking back");

        // Seek forward first.
        decoder.seek(Duration::from_secs(3)).unwrap();
        let mut buf2 = [0.0f32; 1024];
        let n_mid = decoder.read(&mut buf2);
        assert!(n_mid > 0, "should read after seek forward");

        // Seek backward to beginning.
        decoder.seek(Duration::from_secs(0)).unwrap();
        assert!(!decoder.is_eof(), "should not be EOF after seek to start");

        // Read after seeking backward.
        let n_start = decoder.read(&mut buf2);
        assert!(n_start > 0, "should read samples after seek to start");
    })
    .await
    .unwrap();
}

/// Decoder<Stream<File>> multiple seeks in sequence.
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decoder_file_seek_multiple(#[future] server: AudioTestServer, temp_dir: TempDir) {
    let server = server.await;
    let url = server.mp3_url();

    let file_config = FileConfig::new(url.into())
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_look_ahead_bytes(None);
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
    let mut decoder = Audio::<Stream<File>>::new(config).await.unwrap();

    tokio::task::spawn_blocking(move || {
        let mut buf = [0.0f32; 512];

        // Read initial.
        let n = decoder.read(&mut buf);
        assert!(n > 0);

        // Seek forward to 1s, read.
        decoder.seek(Duration::from_secs(1)).unwrap();
        let n = decoder.read(&mut buf);
        assert!(n > 0, "should read after seek to 1s");

        // Seek forward to 5s, read.
        decoder.seek(Duration::from_secs(5)).unwrap();
        let n = decoder.read(&mut buf);
        assert!(n > 0, "should read after seek to 5s");

        // Seek backward to 2s, read.
        decoder.seek(Duration::from_secs(2)).unwrap();
        let n = decoder.read(&mut buf);
        assert!(n > 0, "should read after seek back to 2s");

        // Seek to beginning, read.
        decoder.seek(Duration::from_secs(0)).unwrap();
        let n = decoder.read(&mut buf);
        assert!(n > 0, "should read after seek to 0s");
    })
    .await
    .unwrap();
}

/// Decoder<Stream<File>> events are emitted on seek.
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decoder_file_seek_emits_events(#[future] server: AudioTestServer, temp_dir: TempDir) {
    let server = server.await;
    let url = server.mp3_url();

    let (events_tx, mut events_rx) =
        tokio::sync::broadcast::channel::<AudioPipelineEvent<kithara_file::FileEvent>>(64);

    let file_config = FileConfig::new(url.into())
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_look_ahead_bytes(None);
    let config = AudioConfig::<File>::new(file_config)
        .with_hint("mp3")
        .with_events(events_tx);
    let mut decoder = Audio::<Stream<File>>::new(config).await.unwrap();

    // Read + seek in blocking thread.
    tokio::task::spawn_blocking(move || {
        let mut buf = [0.0f32; 1024];

        // Read to trigger FormatDetected.
        let n = decoder.read(&mut buf);
        assert!(n > 0);

        // Seek and read again.
        decoder.seek(Duration::from_secs(2)).unwrap();
        let n = decoder.read(&mut buf);
        assert!(n > 0);
    })
    .await
    .unwrap();

    // Collect events.
    let mut got_format = false;
    let mut got_seek = false;
    while let Ok(ev) = events_rx.try_recv() {
        match ev {
            AudioPipelineEvent::Audio(AudioEvent::FormatDetected { .. }) => got_format = true,
            AudioPipelineEvent::Audio(AudioEvent::SeekComplete { .. }) => got_seek = true,
            _ => {}
        }
    }
    assert!(got_format, "should have received FormatDetected event");
    assert!(got_seek, "should have received SeekComplete event");
}
