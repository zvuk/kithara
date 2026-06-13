#![forbid(unsafe_code)]

use std::error::Error;

use kithara::{
    assets::StoreOptions,
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::{
    PackagedTestServer, TestTempDir,
    fixture_protocol::{HlsRouteKind, HttpErrorRule},
    temp_dir,
};
use kithara_platform::{CancelToken, time::Duration};

#[kithara::test(
    flash(false),
    tokio,
    timeout(Duration::from_secs(2)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1"),
    tracing("kithara_hls=debug,kithara_stream=info,warn")
)]
async fn prefetch_403_returns_err_quickly(
    temp_dir: TestTempDir,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = PackagedTestServer::with_error_rules(vec![HttpErrorRule {
        kind: HlsRouteKind::Key,
        status: 403,
        body: Some("{\"detail\":\"User not registered\"}".to_string()),
        ..Default::default()
    }])
    .await;

    let url = server.url("/master-encrypted.m3u8");
    let config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(CancelToken::never())
        .build();

    let started = kithara_platform::time::Instant::now();
    let result =
        kithara_platform::time::timeout(Duration::from_secs(1), Stream::<Hls>::new(config))
            .await
            .map_err(|_| "Stream::<Hls>::new did not return within 1s — silent hang regression")?;
    let elapsed = started.elapsed();

    let err = match result {
        Ok(_) => panic!(
            "Stream::<Hls>::new must fail when key server returns 403; got Ok in {elapsed:?}"
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
