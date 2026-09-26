use std::{
    io::{Read, Seek, SeekFrom},
    sync::Mutex as StdMutex,
};

use kithara::{
    abr::AbrEvent,
    assets::{AssetStore, StorageBackend},
    events::EventBus,
    hls::{AbrMode, Hls, HlsConfig, HlsEvent},
    platform::{
        CancelToken,
        sync::Arc,
        time::Duration,
        tokio::task::{spawn, spawn_blocking},
    },
    stream::Stream,
};
use kithara_integration_tests::{
    CreatedHls, TestServerHelper, auto,
    bufpool_ext::{TestPools, pools},
    event::TestEvent,
    hls_server::{abr_binary_ladder, test_pattern_hls},
};
use kithara_test_utils::{TestTempDir, cancel_token, temp_dir};
use tracing::info;

/// Driver-1: Verify that seek works AFTER all segments have been downloaded.
///
/// Scenario:
/// 1. Load all 3 segments from playlist (variant 0, fixed ABR)
/// 2. Read all data until EOF (segment stream finishes)
/// 3. Seek back to segment 1 start (offset `200_000`)
/// 4. Verify segment 1 data is readable
///
/// EXPECTED: seek is processed, segment data is read correctly
#[kithara::test(tokio, native, timeout(Duration::from_secs(10)), hang_timeout_secs(1))]
async fn test_driver_seek_after_playlist_finished(
    #[future(awt)] test_pattern_hls: CreatedHls,
    temp_dir: TestTempDir,
    cancel_token: CancelToken,
) {
    let hls = test_pattern_hls;
    let url = hls.master_url();

    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().to_path_buf(),
        })
        .build();
    let config = HlsConfig::for_url(url)
        .store(store)
        .pools(pools)
        .cancel(cancel_token)
        .initial_abr_mode(AbrMode::manual(0))
        .build();

    let mut stream = Stream::<Hls<TestPools>>::new(config).await.unwrap();

    spawn_blocking(move || {
        let mut all_data = Vec::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            all_data.extend_from_slice(&buf[..n]);
        }

        assert!(
            all_data.len() >= 600_000,
            "Should read all 3 segments (~600KB), got {} bytes",
            all_data.len()
        );

        let pos = stream.seek(SeekFrom::Start(200_000)).unwrap();
        assert_eq!(pos, 200_000);

        let mut buf = [0u8; 9];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 9);
        assert_eq!(
            &buf, b"V0-SEG-1:",
            "After seek past EOF to segment 1, should read V0-SEG-1: prefix"
        );
    })
    .await
    .unwrap();
}

/// Driver-2: ABR switch + seek backward.
///
/// Scenario:
/// 1. Start with variant 0, ABR enabled (auto mode)
/// 2. Read data forward (ABR may switch variants based on throughput)
/// 3. Seek backward to beginning
/// 4. Verify data is readable and consistent from the start
///
/// This tests seek backward at the Stream<Hls> level with ABR active,
/// without the full decoder chain.
#[kithara::test(tokio, native, timeout(Duration::from_secs(30)), hang_timeout_secs(1))]
async fn test_driver_abr_seek_backward(temp_dir: TestTempDir, cancel_token: CancelToken) {
    let hls = TestServerHelper::new()
        .await
        .create_hls(abr_binary_ladder(false, Duration::from_secs(2)))
        .await
        .expect("create ABR ladder");

    let url = hls.master_url();

    let bus = EventBus::new(32);
    let mut events_rx = bus.subscribe();
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().to_path_buf(),
        })
        .build();

    let config = HlsConfig::for_url(url)
        .store(store)
        .pools(pools)
        .cancel(cancel_token)
        .events(bus)
        .initial_abr_mode(auto(0))
        .build();

    let mut stream = Stream::<Hls<TestPools>>::new(config).await.unwrap();

    let variant_switches = Arc::new(StdMutex::new(Vec::new()));
    let switches_clone = variant_switches.clone();

    spawn(async move {
        while let Ok(ev) = events_rx.recv().await.map(|env| env.event) {
            match ev {
                TestEvent::Abr(AbrEvent::VariantApplied { from, to, .. }) => {
                    info!("Variant switch: {} -> {}", from, to);
                    switches_clone.lock().unwrap().push((from, to));
                }
                TestEvent::Hls(HlsEvent::EndOfStream) => break,
                _ => {}
            }
        }
    });

    // No settle timer: the blocking `stream.read()` below already waits for the
    // first bytes to land (state-driven), exactly like
    // `test_driver_seek_after_playlist_finished` which reads with no pre-sleep.
    spawn_blocking(move || {
        let mut first_read = vec![0u8; 50_000];
        let n1 = stream.read(&mut first_read).unwrap();
        assert!(n1 > 0, "Should read initial data");

        let initial_prefix = first_read[..n1.min(9)].to_vec();

        let pos = stream.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(pos, 0, "Seek should return to position 0");

        let mut second_read = vec![0u8; 1000];
        let n2 = stream.read(&mut second_read).unwrap();
        assert!(n2 > 0, "Should read data after seeking to beginning");

        let check_len = n2.min(initial_prefix.len());
        assert_eq!(
            &initial_prefix[..check_len],
            &second_read[..check_len],
            "Data at position 0 should be consistent before and after seek"
        );
    })
    .await
    .unwrap();

    let switches = variant_switches.lock().unwrap();
    info!("Variant switches detected: {:?}", *switches);
    drop(switches);
}
