#![forbid(unsafe_code)]

use std::error::Error;

use kithara::{
    assets::{AssetStore, StorageBackend},
    hls::{Hls, HlsConfig},
    platform::{CancelToken, time::Duration},
    stream::Stream,
};
use kithara_integration_tests::{
    PackagedTestServer, TestTempDir,
    bufpool_ext::{TestPools, pools},
    fixture_protocol::{HlsRouteKind, HttpErrorRule},
    temp_dir,
};

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(2)),
    hang_timeout_secs(1),
    tracing("kithara_hls=debug,kithara_stream=info,warn")
)]
async fn prefetch_403_returns_err_quickly(
    temp_dir: TestTempDir,
    #[future(awt)] denied_key: PackagedTestServer,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = denied_key;
    let url = server.url("/master-encrypted.m3u8");
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().to_path_buf(),
        })
        .build();
    let config = HlsConfig::for_url(url)
        .store(store)
        .pools(pools)
        .cancel(CancelToken::never())
        .build();

    let started = kithara::platform::time::Instant::now();
    let result = kithara::platform::time::timeout(
        Duration::from_secs(1),
        Stream::<Hls<TestPools>>::new(config),
    )
    .await
    .map_err(
        |_| "Stream::<Hls<TestPools>>::new did not return within 1s - silent hang regression",
    )?;
    let elapsed = started.elapsed();

    let err = match result {
        Ok(_) => panic!(
            "Stream::<Hls<TestPools>>::new must fail when key server returns 403; got Ok in {elapsed:?}"
        ),
        Err(e) => e,
    };
    let msg = format!("{err:?}");
    assert!(
        msg.contains("Key") || msg.contains("403") || msg.contains("registered"),
        "expected key-related error message, got {msg}"
    );

    Ok(())
}

#[kithara::fixture]
async fn denied_key() -> PackagedTestServer {
    PackagedTestServer::with_error_rules(vec![HttpErrorRule {
        kind: HlsRouteKind::Key,
        status: 403,
        body: Some("{\"detail\":\"User not registered\"}".to_string()),
        ..Default::default()
    }])
    .await
}
