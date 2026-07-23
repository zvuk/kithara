#![forbid(unsafe_code)]

use kithara::{
    hls::{Hls, HlsConfig},
    platform::{CancelToken, time::Duration},
    stream::Stream,
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir, temp_dir,
};

/// CDN soft-error: server returns 200 OK with text/html body.
/// The HLS engine must reject this before caching and return a
/// content-type error — not a decoder parse failure.
#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn html_body_rejected_before_caching(temp_dir: TestTempDir) {
    let helper = TestServerHelper::new().await;
    let handle = helper.register_behavior(FixtureBehavior {
        content: Content::HtmlError("<html><body>503 Service Unavailable</body></html>"),
        delivery: Delivery::Normal,
    });

    let config = HlsConfig::for_url(handle.url())
        .store(kithara_integration_tests::disk_asset_store(temp_dir.path()))
        .cancel(CancelToken::never())
        .build();

    let result = Stream::<Hls>::new(config).await;
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("HTML body from CDN must be rejected"),
    };

    let msg = format!("{err}");
    assert!(
        msg.contains("content-type")
            || msg.contains("text/html")
            || msg.contains("invalid content"),
        "expected content-type rejection, got: {msg}"
    );
}
