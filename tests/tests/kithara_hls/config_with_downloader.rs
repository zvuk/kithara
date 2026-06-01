use std::{io::Read, time::Duration};

use kithara::{
    assets::StoreOptions,
    hls::{AbrMode, Hls, HlsConfig},
    stream::{
        Stream,
        dl::{Downloader, DownloaderConfig},
    },
};
use kithara_integration_tests::{
    TestTempDir,
    hls_server::abr::{AbrTestServer, master_playlist},
    temp_dir,
};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{CancellationToken, time::sleep, tokio::task::spawn_blocking};

#[kithara::test(
    tokio,
    native,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn hls_config_with_downloader_shares_downloader_across_two_streams(temp_dir: TestTempDir) {
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

    let cancel = CancellationToken::default();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            CancellationToken::default(),
        ))
        .cancel(cancel.child_token())
        .build(),
    );

    let temp_a = temp_dir.path().join("stream_a");
    let temp_b = temp_dir.path().join("stream_b");

    let config_a = HlsConfig::for_url(server_a.url("/master.m3u8"))
        .cancel(cancel.clone())
        .store(StoreOptions::new(&temp_a))
        .initial_abr_mode(AbrMode::manual(0))
        .downloader(downloader.clone())
        .build();
    let config_b = HlsConfig::for_url(server_b.url("/master.m3u8"))
        .cancel(cancel.clone())
        .store(StoreOptions::new(&temp_b))
        .initial_abr_mode(AbrMode::manual(0))
        .downloader(downloader.clone())
        .build();

    let mut stream_a = Stream::<Hls>::new(config_a).await.unwrap();
    let mut stream_b = Stream::<Hls>::new(config_b).await.unwrap();

    sleep(Duration::from_secs(2)).await;

    let read_a = spawn_blocking(move || read_all(&mut stream_a));
    let read_b = spawn_blocking(move || read_all(&mut stream_b));

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
