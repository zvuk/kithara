#![forbid(unsafe_code)]

//! Regression test for seek deadlock when ABR variant switch coincides with seek after EOF.
//!
//! Production scenario:
//!   1. All segments for variant 0 are cached (read to EOF)
//!   2. ABR switches variant 0 → variant 1
//!   3. Seek to middle of stream triggers on-demand request for variant 1 segment
//!   4. `handle_midstream_switch()` drains ALL segment_requests — including the
//!      current-epoch on-demand request
//!   5. Program hangs — request never served
//!
//! Fix: `handle_midstream_switch()` now notifies condvar after draining, and
//! `wait_range` clears `on_demand_pending` when `had_midstream_switch` is true,
//! allowing the reader to re-push its request.

use std::{
    io::{Read, Seek, SeekFrom},
    sync::atomic::Ordering,
};

use kithara::{
    assets::StoreOptions,
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    internal::source_variant_index_handle,
    stream::Stream,
};
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
use kithara_platform::{time::Duration, tokio::task::spawn_blocking};
use kithara_test_utils::{TestTempDir, cancel_token, debug_tracing_setup, temp_dir};
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Seek after ABR variant switch at EOF must not deadlock.
///
/// Reproduces the production bug where `handle_midstream_switch()` unconditionally
/// drains all segment requests, discarding the on-demand request from the new seek epoch.
#[kithara::test(
    tokio,
    native,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn seek_after_variant_switch_at_eof_must_not_deadlock(
    _debug_tracing_setup: (),
    temp_dir: TestTempDir,
    cancel_token: CancellationToken,
) {
    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 3,
        segments_per_variant: 3,
        segment_size: 200_000,
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8").unwrap();

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel_token)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    let mut stream = Stream::<Hls>::new(config).await.unwrap();

    // Get handle to ABR variant index atomic so we can force a switch.
    let variant_index = source_variant_index_handle(stream.source());

    spawn_blocking(move || {
        // Step 1: Read all data to EOF (caches all variant 0 segments).
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
        }
        info!("All variant 0 data read to EOF");

        // Step 2: Force ABR switch to variant 1.
        variant_index.store(1, Ordering::Release);
        info!("ABR variant switched 0 → 1");

        // Step 3: Seek to middle of stream (segment 1 territory).
        let seek_pos = 200_000u64;
        let pos = stream.seek(SeekFrom::Start(seek_pos)).unwrap();
        assert_eq!(pos, seek_pos);
        info!(seek_pos, "Seek applied");

        // Step 4: Read — must return data within the timeout.
        // Before the fix, this deadlocks because the on-demand request
        // for variant 1 segment 1 is silently drained by handle_midstream_switch().
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "Read after seek + variant switch must return data");
        info!(n, "Read succeeded after seek + variant switch");
    })
    .await
    .unwrap();
}
