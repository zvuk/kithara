#![forbid(unsafe_code)]

use std::io::Read;

use kithara::{
    assets::{AssetStore, StorageBackend},
    events::EventBus,
    hls::{AbrMode, Hls, HlsConfig},
    platform::{CancelToken, time::Duration, tokio, tokio::task::spawn_blocking},
    stream::Stream,
};
use kithara_integration_tests::{
    TestTempDir,
    bufpool_ext::{TestPools, pools},
    hls_server::{TestServer, test_server},
    rt_cancel, temp_dir,
};
use tracing::info;

fn read_to_eof_with_progress(stream: &mut Stream<Hls<TestPools>>) -> (Vec<u8>, i32) {
    let mut all_data = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut read_count = 0;
    let mut total_bytes = 0;

    info!("Starting read loop...");
    loop {
        let n = stream.read(&mut buf).unwrap();
        if n == 0 {
            info!("Read returned 0 (EOF), breaking loop");
            break;
        }

        read_count += 1;
        total_bytes += n;
        all_data.extend_from_slice(&buf[..n]);

        if read_count % 100 == 0 {
            info!(
                "Progress: {} reads, {} bytes total",
                read_count, total_bytes
            );
        }

        if read_count > 10000 {
            panic!(
                "Read loop exceeded 10000 iterations. Total bytes: {}, likely infinite loop",
                all_data.len()
            );
        }
    }

    (all_data, read_count)
}

/// Diagnostic version with detailed logging and safety limits
#[kithara::test(
    tokio,
    native,
    timeout(Duration::from_secs(15)),
    hang_timeout_secs(1),
    tracing("kithara_hls=debug,kithara_stream=debug,kithara_decode=debug")
)]
async fn debug_sequential_read(
    #[future(awt)] test_server: TestServer,
    temp_dir: TestTempDir,
    rt_cancel: CancelToken,
) {
    info!("=== Starting debug_sequential_read test ===");

    let server = test_server;
    let url = server.url("/master.m3u8");
    info!("Test server URL: {}", url);

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
        .cancel(rt_cancel)
        .initial_abr_mode(AbrMode::manual(1))
        .events(bus)
        .build();

    info!("Opening HLS stream...");
    let mut stream = Stream::<Hls<TestPools>>::new(config).await.unwrap();

    let events_handle = tokio::task::spawn(async move {
        while let Ok(event) = events_rx.recv().await.map(|env| env.event) {
            info!("HLS Event: {:?}", event);
        }
    });

    info!("Starting blocking read task...");
    let result = spawn_blocking(move || {
        info!("Inside blocking task, starting read");
        let (all_data, read_count) = read_to_eof_with_progress(&mut stream);

        info!(
            "Read loop completed: {} bytes in {} reads",
            all_data.len(),
            read_count
        );
        all_data
    })
    .await
    .unwrap();

    info!("Blocking task completed, received {} bytes", result.len());

    assert!(
        result.len() > 500_000,
        "Should read substantial data, got {} bytes",
        result.len()
    );

    assert!(
        result.starts_with(b"V1-SEG-0:"),
        "Data should start with V1-SEG-0: prefix"
    );

    info!("Read {} bytes total from variant 1", result.len());

    info!("Test passed!");

    drop(events_handle);
}
