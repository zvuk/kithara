#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

//! Proves a forward seek is served by a range request from the seek target
//! instead of waiting for the sequential fetch to walk the skipped span.
use std::{
    convert::Infallible,
    io::{Read, Seek, SeekFrom},
    sync::atomic::{AtomicU64, Ordering},
};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::get,
};
use bytes::Bytes;
use futures::{StreamExt, stream};
use kithara::{
    assets::{AssetStore, StorageBackend},
    file::{File, FileConfig},
    platform::{
        sync::Arc,
        time::{Duration, timeout},
        tokio::task::spawn_blocking,
    },
    stream::Stream,
};
use kithara_integration_tests::{
    TestHttpServer,
    bufpool_ext::{TestPools, pools},
};

const TOTAL: usize = 4096;
/// Bytes the stalled head fetch delivers before it goes quiet forever.
const HEAD_PREFIX: usize = 64;
const SEEK_TO: u64 = 3072;

fn body() -> Vec<u8> {
    (0..TOTAL)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect()
}

#[derive(Clone)]
struct RangeState {
    /// Start offset of the first ranged request, or `u64::MAX` while the server
    /// has only ever been asked for the whole body. First rather than last: the
    /// backfill of the skipped span arrives as a ranged request too, and the
    /// claim under test is that the seek is served before it.
    first_range_start: Arc<AtomicU64>,
}

/// `bytes=start-end` bounds, the inclusive end absent for an open-ended range.
fn range_bounds(headers: &HeaderMap) -> Option<(u64, Option<u64>)> {
    let value = headers.get(header::RANGE)?.to_str().ok()?;
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    let end = if end.is_empty() {
        None
    } else {
        Some(end.parse().ok()?)
    };
    Some((start.parse().ok()?, end))
}

/// The head request stalls after a short prefix and never completes, so the
/// only way to serve a forward seek is a second, ranged request. A ranged
/// request is served exactly as bounded: the backfill of the skipped span asks
/// for a closed range, and the client rejects a `206` that overshoots it.
async fn serve(State(state): State<RangeState>, headers: HeaderMap) -> Response {
    let bytes = body();
    let Some((start, end)) = range_bounds(&headers).filter(|(start, _)| *start > 0) else {
        let prefix = Bytes::copy_from_slice(&bytes[..HEAD_PREFIX]);
        let body = Body::from_stream(
            stream::iter(vec![Ok::<_, Infallible>(prefix)]).chain(stream::pending()),
        );
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, TOTAL.to_string())
            .header(header::CONTENT_TYPE, "audio/mpeg")
            .body(body)
            .expect("stalled head response");
    };

    let _ = state.first_range_start.compare_exchange(
        u64::MAX,
        start,
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
    let start = usize::try_from(start).expect("range start fits usize");
    let end = end.map_or(TOTAL - 1, |end| {
        usize::try_from(end)
            .expect("range end fits usize")
            .min(TOTAL - 1)
    });
    let span = &bytes[start..=end];
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_LENGTH, span.len().to_string())
        .header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{TOTAL}"),
        )
        .header(header::CONTENT_TYPE, "audio/mpeg")
        .body(Body::from(Bytes::copy_from_slice(span)))
        .expect("ranged response")
}

#[kithara::test(
    tokio,
    multi_thread,
    flash(false),
    timeout(Duration::from_secs(20)),
    hang_timeout_secs(5)
)]
async fn a_forward_seek_is_served_by_a_range_request() {
    let first_range_start = Arc::new(AtomicU64::new(u64::MAX));
    let app = Router::new()
        .route("/audio.mp3", get(serve))
        .with_state(RangeState {
            first_range_start: Arc::clone(&first_range_start),
        });
    let server = TestHttpServer::new(app).await;
    let pools = pools();

    let config = FileConfig::for_src(server.url("/audio.mp3").into())
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .pools(pools)
        .build();
    let mut stream = Stream::<File<TestPools>>::new(config)
        .await
        .expect("remote stream");

    let seeked = spawn_blocking(move || {
        let mut head = [0u8; 16];
        stream.read_exact(&mut head).expect("head bytes arrive");
        stream.seek(SeekFrom::Start(SEEK_TO)).expect("seek forward");
        let mut at_target = [0u8; 16];
        stream
            .read_exact(&mut at_target)
            .expect("seeked bytes arrive");
        at_target
    });

    let at_target = timeout(Duration::from_secs(10), seeked)
        .await
        .expect("a forward seek must be served by a range request, not by the stalled head fetch")
        .expect("reader task");

    let expected = body();
    let target = usize::try_from(SEEK_TO).expect("seek target fits usize");
    assert_eq!(
        at_target,
        expected[target..target + 16],
        "the seeked read must return the bytes under the new cursor"
    );
    assert_eq!(
        first_range_start.load(Ordering::SeqCst),
        SEEK_TO,
        "the seek must be fetched with a range request anchored at the seek target"
    );
}
