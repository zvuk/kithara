//! Integration tests: verify `with_ephemeral(true)` routes data through `MemDriver`
//! ring buffer (in-memory) instead of `MmapDriver` (disk).
//!
//! Two levels of verification:
//! 1. **Structural**: `AssetStoreBuilder::ephemeral(true)` creates resources with
//!    `path() == None` (`MemResource`), while disk creates `path() == Some(_)` (`MmapResource`).
//! 2. **Pipeline**: `Audio<Stream<Hls>>` with `ephemeral: true` produces valid audio
//!    and leaves the temp directory empty (no files created on disk).

#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::{fs, path::Path};

#[cfg(not(target_arch = "wasm32"))]
use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara::{
    assets::{AssetStoreBuilder, ResourceKey},
    storage::ResourceExt,
};
#[cfg(not(target_arch = "wasm32"))]
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
use kithara_platform::Duration;
#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::tokio::task::spawn_blocking;
#[cfg(not(target_arch = "wasm32"))]
use kithara_test_utils::TestTempDir;
#[cfg(not(target_arch = "wasm32"))]
use kithara_test_utils::create_wav_exact_bytes;
use kithara_test_utils::signal_pcm::signal;
#[cfg(not(target_arch = "wasm32"))]
use tokio_util::sync::CancellationToken;
#[cfg(not(target_arch = "wasm32"))]
use tracing::info;

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn ephemeral_backend_creates_mem_resource() {
    let backend = AssetStoreBuilder::new()
        .ephemeral(true)
        .asset_root(Some("test"))
        .build();

    let key = ResourceKey::new("seg_0.m4s");
    let res = backend
        .acquire_resource(&key)
        .expect("open ephemeral resource");
    assert!(
        res.path().is_none(),
        "ephemeral resource must not have file path (MemResource)"
    );
}

#[kithara::test(
    native,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
fn disk_backend_creates_mmap_resource() {
    let temp = TestTempDir::new();
    let backend = AssetStoreBuilder::new()
        .root_dir(temp.path())
        .asset_root(Some("test"))
        .ephemeral(false)
        .build();

    let key = ResourceKey::new("seg_0.m4s");
    let res = backend.acquire_resource(&key).expect("open disk resource");
    assert!(
        res.path().is_some(),
        "disk resource must have file path (MmapResource)"
    );
}

use crate::common::test_defaults::SawWav;

/// Recursively count files inside a directory tree.
#[cfg(not(target_arch = "wasm32"))]
fn count_files(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
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

#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_hls=debug,kithara_stream=debug")
)]
async fn ephemeral_pipeline_no_disk_writes() {
    /// Keep within default LRU cache capacity (5) to avoid auto-eviction of
    /// `MemResources` which would make `wait_range()` block forever.
    const SEGMENT_COUNT: usize = 3;
    const TOTAL_BYTES: usize = SEGMENT_COUNT * SawWav::DEFAULT.segment_size;

    // Generate saw-tooth WAV
    let wav_data = create_wav_exact_bytes(
        signal::Sawtooth,
        SawWav::DEFAULT.sample_rate,
        SawWav::DEFAULT.channels,
        TOTAL_BYTES,
    );
    info!(total_bytes = TOTAL_BYTES, "Generated saw-tooth WAV");

    // Spawn HLS server
    let segment_duration = SawWav::DEFAULT.segment_size as f64
        / (f64::from(SawWav::DEFAULT.sample_rate) * f64::from(SawWav::DEFAULT.channels) * 2.0);
    let server = HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SawWav::DEFAULT.segment_size,
        segment_duration_secs: segment_duration,
        custom_data: Some(Arc::new(wav_data)),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8");
    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::new();

    // Create an Audio pipeline with ephemeral=true
    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()).with_ephemeral(true))
        .with_cancel(cancel)
        .with_abr_options(AbrOptions {
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
    let result = spawn_blocking(move || {
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
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
