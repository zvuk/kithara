#![forbid(unsafe_code)]

use axum::{Router, response::IntoResponse, routing::get};
use kithara::{
    assets::StoreOptions,
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_platform::time::Duration;
use kithara_test_utils::{TestTempDir, temp_dir};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use url::Url;

async fn html_stub() -> impl IntoResponse {
    (
        [("content-type", "text/html; charset=utf-8")],
        "<html><body>503 Service Unavailable</body></html>",
    )
}

async fn start_html_stub_server() -> u16 {
    let app = Router::new().fallback(get(html_stub));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    port
}

/// CDN soft-error: server returns 200 OK with text/html body.
/// The HLS engine must reject this before caching and return a
/// content-type error — not a decoder parse failure.
#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn html_body_rejected_before_caching(temp_dir: TestTempDir) {
    let port = start_html_stub_server().await;
    let url = Url::parse(&format!("http://127.0.0.1:{port}/master.m3u8")).unwrap();

    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(CancellationToken::new());

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
