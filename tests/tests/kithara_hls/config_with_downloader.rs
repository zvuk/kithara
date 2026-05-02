//! Integration test: [`HlsConfig::with_downloader`] lets two HLS streams
//! share a single `kithara_stream::dl::Downloader`.
//!
//! Protects the phase-02 migration API contract: `Downloader` is the
//! sole `HttpClient` owner, and callers can pass the same instance to
//! multiple HLS streams via `HlsConfig::with_downloader(dl.clone())`.

use std::{io::Read, time::Duration};

use kithara::{
    assets::StoreOptions,
    hls::{AbrMode, Hls, HlsConfig},
    stream::{
        Stream,
        dl::{Downloader, DownloaderConfig},
    },
};
use kithara_integration_tests::hls_fixture::abr::{AbrTestServer, master_playlist};
use kithara_platform::{time::sleep, tokio::task::spawn_blocking};
use kithara_test_utils::{TestTempDir, temp_dir};
use tokio_util::sync::CancellationToken;

#[kithara::test(
    tokio,
    native,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn hls_config_with_downloader_shares_downloader_across_two_streams(temp_dir: TestTempDir) {
    // Two AbrTestServers isolate per-stream state — each serves its own
    // master playlist with 3 segments, variant 0 at 256 kbps.
    let server_a = AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_millis(10),
    )
    .await;
    let server_b = AbrTestServer::new(
        master_playlist(256_000, 512_000, 1_024_000),
        false,
        Duration::from_millis(10),
    )
    .await;

    // Single shared Downloader. `dl.clone()` is cheap (Arc inside) and
    // is the whole point of the phase-02 `with_downloader` API.
    let cancel = CancellationToken::new();
    let downloader = Downloader::new(DownloaderConfig::default().with_cancel(cancel.child_token()));

    let temp_a = temp_dir.path().join("stream_a");
    let temp_b = temp_dir.path().join("stream_b");

    let config_a = HlsConfig::new(server_a.url("/master.m3u8"))
        .with_cancel(cancel.clone())
        .with_store(StoreOptions::new(&temp_a))
        .with_initial_abr_mode(AbrMode::Manual(0))
        .with_downloader(downloader.clone());
    let config_b = HlsConfig::new(server_b.url("/master.m3u8"))
        .with_cancel(cancel.clone())
        .with_store(StoreOptions::new(&temp_b))
        .with_initial_abr_mode(AbrMode::Manual(0))
        .with_downloader(downloader.clone());

    let mut stream_a = Stream::<Hls>::new(config_a).await.unwrap();
    let mut stream_b = Stream::<Hls>::new(config_b).await.unwrap();

    // Give both streams time to fetch segments.
    sleep(Duration::from_secs(2)).await;

    // Read both streams to EOF in parallel blocking tasks.
    let read_a = spawn_blocking(move || read_all(&mut stream_a));
    let read_b = spawn_blocking(move || read_all(&mut stream_b));

    let bytes_a = read_a.await.unwrap();
    let bytes_b = read_b.await.unwrap();

    // Both streams must have read substantial data (>= 500KB — 3
    // segments × ~200KB each, same as sync_reader_hls_test).
    assert!(
        bytes_a.len() >= 500_000,
        "stream A only read {} bytes (expected >= 500_000)",
        bytes_a.len()
    );
    assert!(
        bytes_b.len() >= 500_000,
        "stream B only read {} bytes (expected >= 500_000)",
        bytes_b.len()
    );

    // Both streams use the same test-fixture byte layout (9-byte
    // header + data per segment) — identical byte counts are expected.
    assert_eq!(
        bytes_a.len(),
        bytes_b.len(),
        "shared-downloader streams produced different total byte counts",
    );

    cancel.cancel();
}

fn read_all(stream: &mut Stream<Hls>) -> Vec<u8> {
    let mut all = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => all.extend_from_slice(&buf[..n]),
            Err(e) => panic!("read error after {} bytes: {e}", all.len()),
        }
        if all.len() > 10_000_000 {
            panic!("read too much data: {} bytes", all.len());
        }
    }
    all
}
