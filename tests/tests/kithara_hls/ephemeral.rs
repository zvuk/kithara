//! Integration tests: verify `with_ephemeral(true)` routes data through `MemDriver`
//! ring buffer (in-memory) instead of `MmapDriver` (disk).
//!
//! Two levels of verification:
//! 1. **Structural**: `AssetStoreBuilder::ephemeral(true)` creates resources with
//!    `path() == None` (MemResource), while disk creates `path() == Some(_)` (MmapResource).
//! 2. **Pipeline**: `Audio<Stream<Hls>>` with `ephemeral: true` produces valid audio
//!    and leaves the temp directory empty (no files created on disk).

use std::{sync::Arc, time::Duration};

use kithara::{
    assets::{AssetStoreBuilder, StoreOptions},
    audio::{Audio, AudioConfig},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    storage::ResourceExt,
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_test_utils::wav::create_saw_wav;
use rstest::rstest;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::fixture::{HlsTestServer, HlsTestServerConfig};

#[rstest]
#[timeout(Duration::from_secs(5))]
fn ephemeral_backend_creates_mem_resource() {
    let backend = AssetStoreBuilder::new()
        .ephemeral(true)
        .asset_root(Some("test"))
        .build();

    let key = kithara::assets::ResourceKey::new("seg_0.m4s");
    let res = backend
        .open_resource(&key)
        .expect("open ephemeral resource");
    assert!(
        res.path().is_none(),
        "ephemeral resource must not have file path (MemResource)"
    );
}

#[rstest]
#[timeout(Duration::from_secs(5))]
fn disk_backend_creates_mmap_resource() {
    let temp = TempDir::new().expect("temp dir");
    let backend = AssetStoreBuilder::new()
        .root_dir(temp.path())
        .asset_root(Some("test"))
        .ephemeral(false)
        .build();

    let key = kithara::assets::ResourceKey::new("seg_0.m4s");
    let res = backend.open_resource(&key).expect("open disk resource");
    assert!(
        res.path().is_some(),
        "disk resource must have file path (MmapResource)"
    );
}

const SAMPLE_RATE: u32 = 44100;
const CHANNELS: u16 = 2;
const SEGMENT_SIZE: usize = 200_000;
/// Keep within default LRU cache capacity (5) to avoid auto-eviction of
/// MemResources which would make `wait_range()` block forever.
const SEGMENT_COUNT: usize = 3;
const TOTAL_BYTES: usize = SEGMENT_COUNT * SEGMENT_SIZE;

/// Recursively count files inside a directory tree.
fn count_files(dir: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut count = 0;
    for entry in entries.flatten() {
        let ft = entry.file_type();
        if let Ok(ft) = ft {
            if ft.is_file() {
                count += 1;
            } else if ft.is_dir() {
                count += count_files(&entry.path());
            }
        }
    }
    count
}

#[rstest]
#[timeout(Duration::from_secs(60))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ephemeral_pipeline_no_disk_writes() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| {
            "kithara_audio=debug,kithara_decode=debug,kithara_hls=debug,kithara_stream=debug"
                .to_string()
        }))
        .try_init();

    // Generate saw-tooth WAV
    let wav_data = create_saw_wav(TOTAL_BYTES);
    info!(total_bytes = TOTAL_BYTES, "Generated saw-tooth WAV");

    // Spawn HLS server
    let segment_duration = SEGMENT_SIZE as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 2.0);
    let server = HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data: Some(Arc::new(wav_data)),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8").expect("url");
    let temp_dir = TempDir::new().expect("temp dir");
    let cancel = CancellationToken::new();

    // Create Audio pipeline with ephemeral=true
    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()).with_ephemeral(true))
        .with_cancel(cancel)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>> pipeline");

    // Read samples in blocking thread
    let temp_path = temp_dir.path().to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        let mut buf = vec![0.0f32; 4096];
        let mut total_samples = 0usize;

        for _ in 0..100 {
            let n = audio.read(&mut buf);
            if n == 0 {
                break;
            }
            total_samples += n;

            // Verify sample integrity
            for &s in &buf[..n] {
                assert!(
                    s.is_finite() && (-1.0..=1.0).contains(&s),
                    "invalid sample: {s}"
                );
            }
        }

        assert!(total_samples > 0, "must read some audio samples");
        info!(total_samples, "Read audio samples");

        // Verify NO files on disk — proves MemDriver, not MmapDriver
        let file_count = count_files(&temp_path);
        assert_eq!(
            file_count, 0,
            "ephemeral mode must not create files on disk, found {file_count}"
        );

        info!("Verified: no disk files created in ephemeral mode");
    })
    .await;

    match result {
        Ok(()) => info!("Ephemeral pipeline test passed"),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
