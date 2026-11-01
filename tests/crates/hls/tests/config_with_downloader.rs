use std::io::Read;

use kithara::{
    assets::{AssetStore, StorageBackend},
    download::{Downloader, DownloaderConfig, DownloaderEvent},
    events::EventBus,
    hls::{AbrMode, Hls, HlsConfig},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{self, Duration},
        tokio::task::spawn_blocking,
    },
    stream::Stream,
};
use kithara_integration_tests::{
    TestServerHelper,
    bufpool_ext::{TestPools, pools},
    hls_server::abr_binary_ladder,
};
use kithara_test_utils::{TestTempDir, temp_dir};

#[kithara::test(
    tokio,
    native,
    flash(false),
    timeout(Duration::from_secs(20)),
    hang_timeout_secs(1)
)]
async fn hls_config_with_downloader_shares_downloader_across_two_streams(temp_dir: TestTempDir) {
    let helper = TestServerHelper::new().await;
    let ladder = abr_binary_ladder(false, Duration::ZERO);
    let hls_a = helper
        .create_hls(ladder.clone())
        .await
        .expect("create ABR ladder");
    let hls_b = helper.create_hls(ladder).await.expect("create ABR ladder");
    let gates = [
        helper.register_segment_gate(hls_a.token(), 0, 0),
        helper.register_segment_gate(hls_b.token(), 0, 0),
    ];

    let cancel = CancelToken::never();
    let pools = pools();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools.clone(),
            CancelToken::never(),
        ))
        .runtime(kithara::platform::tokio::runtime::Handle::current())
        .soft_timeout(Duration::ZERO)
        .cancel(cancel.child())
        .build(),
    );

    let temp_a = temp_dir.path().join("stream_a");
    let temp_b = temp_dir.path().join("stream_b");
    let store_a = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk { root: temp_a })
        .build();
    let store_b = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk { root: temp_b })
        .build();

    let bus_a = EventBus::new(1024);
    let bus_b = EventBus::new(1024);
    let mut events_a = bus_a.subscribe::<DownloaderEvent>();
    let mut events_b = bus_b.subscribe::<DownloaderEvent>();
    let config_a = HlsConfig::for_url(hls_a.master_url())
        .cancel(cancel.clone())
        .events(bus_a)
        .store(store_a)
        .pools(pools.clone())
        .initial_abr_mode(AbrMode::manual(0))
        .downloader(downloader.clone())
        .build();
    let config_b = HlsConfig::for_url(hls_b.master_url())
        .cancel(cancel.clone())
        .events(bus_b)
        .store(store_b)
        .pools(pools.clone())
        .initial_abr_mode(AbrMode::manual(0))
        .downloader(downloader.clone())
        .build();

    let mut stream_a = Stream::<Hls<TestPools>>::new(config_a).await.unwrap();
    let mut stream_b = Stream::<Hls<TestPools>>::new(config_b).await.unwrap();

    let read_a = spawn_blocking(move || read_all(&mut stream_a));
    let read_b = spawn_blocking(move || read_all(&mut stream_b));
    let reported = time::timeout(Duration::from_millis(250), async {
        let mut reported = [false; 2];
        for (events, reported) in [&mut events_a, &mut events_b]
            .into_iter()
            .zip(&mut reported)
        {
            while let Ok(envelope) = events.recv().await {
                *reported |= matches!(envelope.event, DownloaderEvent::LoadSlow { .. });
                if *reported {
                    break;
                }
            }
        }
        reported
    })
    .await;
    for gate in gates {
        gate.release();
    }

    let bytes_a = read_a.await.unwrap();
    let bytes_b = read_b.await.unwrap();

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

    assert_eq!(
        bytes_a.len(),
        bytes_b.len(),
        "shared-downloader streams produced different total byte counts",
    );

    for reported in reported.expect("both injected downloader observations stay within the budget")
    {
        assert!(reported, "each stream must use the injected Downloader");
    }

    cancel.cancel();
}

fn read_all(stream: &mut Stream<Hls<TestPools>>) -> Vec<u8> {
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
