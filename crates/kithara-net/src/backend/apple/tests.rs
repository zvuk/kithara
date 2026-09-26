mod kithara {
    pub(crate) use kithara_test_macros::test;
}

use std::{
    io,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

use axum::{
    Router,
    body::Body,
    handler::Handler,
    http::{HeaderMap, StatusCode, response::Builder},
    response::Response,
    routing::any,
};
use bytes::Bytes;
use futures::{StreamExt, stream};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
    time::Duration,
    tokio::task::spawn,
};
use kithara_test_utils::TestHttpServer;

use super::client::AppleNet;
use crate::{
    error::NetError,
    test_pools::pools,
    types::{Compression, Headers, NetOptions, RangeSpec, RetryPolicy},
};

const PROBE: &str = "/probe";
const PLAYLIST: &[u8] = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\na.m3u8\n";
const GZIP_PLAYLIST: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x53, 0x76, 0x8d, 0x08, 0xf1, 0x35,
    0x0e, 0xe5, 0x52, 0x06, 0xd2, 0xba, 0x11, 0xba, 0xc1, 0x21, 0x41, 0xae, 0x8e, 0xbe, 0xba, 0x9e,
    0x7e, 0x6e, 0x56, 0x4e, 0x8e, 0x7e, 0x2e, 0xe1, 0x9e, 0x2e, 0x21, 0x1e, 0xb6, 0x86, 0x5c, 0x89,
    0x7a, 0xb9, 0xc6, 0xa5, 0x16, 0x5c, 0x00, 0xf1, 0x51, 0x3e, 0xd3, 0x2d, 0x00, 0x00, 0x00,
];

/// Serves `handler` for every method on [`PROBE`].
async fn serve<H, T>(handler: H) -> TestHttpServer
where
    H: Handler<T, ()>,
    T: 'static,
{
    TestHttpServer::new(Router::new().route(PROBE, any(handler))).await
}

fn respond(status: StatusCode, headers: &[(&str, &str)], body: impl Into<Body>) -> Response {
    headers
        .iter()
        .fold(
            Response::builder().status(status),
            |builder, (key, value)| builder.header(*key, *value),
        )
        .body(body.into())
        .expect("test response")
}

/// A body of unknown length: chunked on a GET, and on a HEAD a response that
/// carries no `Content-Length` at all.
fn unsized_body(chunks: &'static [&'static [u8]]) -> Body {
    Body::from_stream(stream::iter(
        chunks
            .iter()
            .map(|chunk| Ok::<_, io::Error>(Bytes::from_static(chunk))),
    ))
}

/// Sends `sent`, holds the connection for `linger`, then drops it mid-body.
fn truncated(head: Builder, sent: Bytes, linger: Duration) -> Response {
    let dropped = stream::once(async move {
        kithara_platform::time::sleep(linger).await;
        Err::<Bytes, _>(io::Error::other("connection dropped mid-body"))
    });
    head.body(Body::from_stream(stream::iter([Ok(sent)]).chain(dropped)))
        .expect("truncated response")
}

/// Sends the head and never a byte of the body.
fn stalled_head() -> Response {
    Response::new(Body::from_stream(
        stream::pending::<Result<Bytes, io::Error>>(),
    ))
}

fn fast_options(max_retries: u32) -> NetOptions {
    NetOptions::builder()
        .retry_policy(RetryPolicy {
            max_retries,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        })
        .inactivity_timeout(Duration::from_millis(30))
        .build()
}

fn stream_options(inactivity_ms: u64) -> NetOptions {
    NetOptions::builder()
        .retry_policy(RetryPolicy {
            max_retries: 0,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        })
        .inactivity_timeout(Duration::from_millis(inactivity_ms))
        .build()
}

fn range_start(headers: &HeaderMap) -> Option<usize> {
    request_header(headers, "range")
        .and_then(|range| range.strip_prefix("bytes="))
        .and_then(|range| range.split_once('-').map(|(start, _)| start))
        .and_then(|start| start.parse().ok())
}

fn request_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn overriding_accept_encoding() -> Headers {
    let mut headers = Headers::default();
    headers.insert("AcCePt-EnCoDiNg", "br");
    headers
}

async fn collect(mut stream: crate::ByteStream) -> Result<Bytes, NetError> {
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(chunk?.as_ref());
    }
    Ok(Bytes::from(out))
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_get_bytes_retries_503_until_ok() {
    let counter = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&counter);
    let server = serve(move || {
        let seen = Arc::clone(&seen);
        async move {
            let attempt = seen.fetch_add(1, Ordering::SeqCst);
            if attempt < 2 {
                respond(StatusCode::SERVICE_UNAVAILABLE, &[], "busy")
            } else {
                respond(StatusCode::OK, &[], "ok")
            }
        }
    })
    .await;
    let url = server.url(PROBE);

    let client = AppleNet::new(fast_options(3), pools(), CancelToken::never());
    let body = client
        .get_bytes(url, None)
        .await
        .expect("get_bytes retries 503");

    assert_eq!(&body[..], b"ok");
    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_accept_encoding_policy_is_authoritative_per_request() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler_seen = Arc::clone(&seen);
    let server = serve(move |headers: HeaderMap| {
        let seen = Arc::clone(&handler_seen);
        async move {
            let accept_encoding = request_header(&headers, "accept-encoding")
                .unwrap_or_default()
                .to_string();
            seen.lock().push(accept_encoding);
            respond(StatusCode::OK, &[], "ok")
        }
    })
    .await;
    let url = server.url(PROBE);

    let options = NetOptions::builder()
        .compression(Compression::GZIP | Compression::DEFLATE)
        .build();
    let client = AppleNet::new(options, pools(), CancelToken::never());

    let body = client
        .get_bytes(url.clone(), Some(overriding_accept_encoding()))
        .await
        .expect("whole-body get");
    assert_eq!(&body[..], b"ok");

    let stream = client
        .stream(url.clone(), Some(overriding_accept_encoding()))
        .await
        .expect("stream");
    assert_eq!(&collect(stream).await.expect("stream body")[..], b"ok");

    let range = client
        .get_range(
            url.clone(),
            RangeSpec::new(0, Some(1)),
            Some(overriding_accept_encoding()),
        )
        .await
        .expect("range");
    assert_eq!(&collect(range).await.expect("range body")[..], b"ok");

    client
        .head(url.clone(), Some(overriding_accept_encoding()))
        .await
        .expect("head");
    let body = client
        .post_bytes(
            url,
            Bytes::from_static(b"request"),
            Some(overriding_accept_encoding()),
        )
        .await
        .expect("whole-body post");
    assert_eq!(&body[..], b"ok");

    assert_eq!(
        seen.lock().as_slice(),
        [
            "gzip, deflate",
            "identity",
            "identity",
            "identity",
            "gzip, deflate"
        ]
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_whole_body_preserves_configured_auto_decode() {
    let server = serve(|headers: HeaderMap| async move {
        assert_eq!(request_header(&headers, "accept-encoding"), Some("gzip"));
        respond(
            StatusCode::OK,
            &[("Content-Encoding", "gzip")],
            GZIP_PLAYLIST,
        )
    })
    .await;
    let url = server.url(PROBE);
    let options = NetOptions::builder().compression(Compression::GZIP).build();
    let client = AppleNet::new(options, pools(), CancelToken::never());

    let body = client
        .get_bytes(url, None)
        .await
        .expect("Foundation auto-decodes configured gzip");

    assert_eq!(&body[..], PLAYLIST);
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_identity_stream_rejects_nonidentity_content_encoding() {
    let server = serve(|| async {
        respond(
            StatusCode::OK,
            &[("CoNtEnT-EnCoDiNg", "gzip")],
            GZIP_PLAYLIST,
        )
    })
    .await;
    let url = server.url(PROBE);
    let client = AppleNet::new(fast_options(0), pools(), CancelToken::never());

    let Err(error) = client.stream(url.clone(), None).await else {
        panic!("encoded bytes must not reach an identity stream");
    };

    assert!(
        matches!(error, NetError::Decode(detail) if detail.contains(url.as_str()) && detail.contains("gzip"))
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_head_backfills_content_length_from_content_range() {
    let server = serve(|| async {
        respond(
            StatusCode::PARTIAL_CONTENT,
            &[("Content-Range", "bytes 0-0/1234")],
            unsized_body(&[]),
        )
    })
    .await;

    let client = AppleNet::new(fast_options(0), pools(), CancelToken::never());
    let headers = client.head(server.url(PROBE), None).await.expect("head");

    assert_eq!(headers.get("content-length"), Some("1234"));
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_range_rejects_partial_without_content_range() {
    let server = serve(|| async { respond(StatusCode::PARTIAL_CONTENT, &[], "part") }).await;
    let url = server.url(PROBE);
    let client = AppleNet::new(fast_options(0), pools(), CancelToken::never());

    let Err(error) = client
        .get_range(url, RangeSpec::new(0, Some(3)), None)
        .await
    else {
        panic!("partial range without content-range must fail before body delivery");
    };

    assert!(matches!(error, NetError::Decode(detail) if detail.contains("content-range")));
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_range_rejects_partial_without_content_length() {
    let server = serve(|| async {
        respond(
            StatusCode::PARTIAL_CONTENT,
            &[("Content-Range", "bytes 0-3/8")],
            unsized_body(&[b"abcdefgh"]),
        )
    })
    .await;
    let url = server.url(PROBE);
    let client = AppleNet::new(fast_options(0), pools(), CancelToken::never());

    let Err(error) = client
        .get_range(url, RangeSpec::new(0, Some(3)), None)
        .await
    else {
        panic!("partial range without content-length must fail before body delivery");
    };

    assert!(matches!(error, NetError::Decode(detail) if detail.contains("content-length")));
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_resume_rejects_a_different_content_range() {
    const BODY_LEN: usize = 2 * 1024 * 1024;
    const FIRST_WRITE_LEN: usize = 1024 * 1024;

    let body = Bytes::from(vec![0x5a; BODY_LEN]);
    let requests = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&requests);
    let server = serve(move |headers: HeaderMap| {
        let seen = Arc::clone(&seen);
        let body = body.clone();
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            if range_start(&headers).is_some() {
                let content_range = format!("bytes 0-0/{}", body.len());
                respond(
                    StatusCode::PARTIAL_CONTENT,
                    &[("Content-Range", &content_range)],
                    "x",
                )
            } else {
                truncated(
                    Response::builder().header("Content-Length", body.len()),
                    body.slice(..FIRST_WRITE_LEN),
                    Duration::from_millis(20),
                )
            }
        }
    })
    .await;
    let client = AppleNet::new(fast_options(1), pools(), CancelToken::never());
    let stream = client
        .stream(server.url(PROBE), None)
        .await
        .expect("initial stream");

    let error = collect(stream)
        .await
        .expect_err("wrong resumed range must terminate the stream");

    assert!(
        matches!(error, NetError::Decode(detail) if detail.contains("bytes=") && detail.contains("bytes 0-0/"))
    );
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_resume_rejects_a_conflicting_representation_total() {
    const BODY_LEN: usize = 2 * 1024 * 1024;
    const FIRST_WRITE_LEN: usize = 1024 * 1024;

    let body = Bytes::from(vec![0x5a; BODY_LEN]);
    let requests = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&requests);
    let server = serve(move |headers: HeaderMap| {
        let seen = Arc::clone(&seen);
        let body = body.clone();
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            range_start(&headers).map_or_else(
                || {
                    truncated(
                        Response::builder().header("Content-Length", body.len()),
                        body.slice(..FIRST_WRITE_LEN),
                        Duration::from_millis(20),
                    )
                },
                |start| {
                    let end = body.len().saturating_sub(1);
                    let conflicting_total = body.len().saturating_add(1);
                    let content_range = format!("bytes {start}-{end}/{conflicting_total}");
                    respond(
                        StatusCode::PARTIAL_CONTENT,
                        &[("Content-Range", &content_range)],
                        body.slice(start..),
                    )
                },
            )
        }
    })
    .await;
    let client = AppleNet::new(fast_options(1), pools(), CancelToken::never());
    let stream = client
        .stream(server.url(PROBE), None)
        .await
        .expect("initial stream");

    let error = collect(stream)
        .await
        .expect_err("conflicting resumed total must terminate the stream");

    assert!(
        matches!(error, NetError::Decode(detail) if detail.contains("changed representation total"))
    );
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_ignored_range_resume_continues_after_discovering_total() {
    const BODY_BYTE: u8 = 0x5a;
    const BODY_LEN: usize = 2 * 1024 * 1024;
    const FIRST_WRITE_LEN: usize = 1024 * 1024;
    const REQUEST_END: u64 = 1023;

    let body = Bytes::from(vec![BODY_BYTE; BODY_LEN]);
    let requests = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&requests);
    let server = serve(move |headers: HeaderMap| {
        let body = body.clone();
        let attempt = seen.fetch_add(1, Ordering::SeqCst);
        async move {
            if attempt > 0 {
                let start = range_start(&headers).expect("resumed range start");
                let range = request_header(&headers, "range").expect("range header");
                assert_eq!(range, format!("bytes={start}-"));
                let end_exclusive = BODY_LEN;
                let end = end_exclusive.saturating_sub(1);
                let content_range = format!("bytes {start}-{end}/{BODY_LEN}");
                respond(
                    StatusCode::PARTIAL_CONTENT,
                    &[("Content-Range", &content_range)],
                    body.slice(start..end_exclusive),
                )
            } else {
                truncated(
                    Response::builder(),
                    body.slice(..FIRST_WRITE_LEN),
                    Duration::from_millis(60),
                )
            }
        }
    })
    .await;
    let client = AppleNet::new(fast_options(2), pools(), CancelToken::never());
    let stream = client
        .get_range(
            server.url(PROBE),
            RangeSpec::new(0, Some(REQUEST_END)),
            None,
        )
        .await
        .expect("initial stream");

    let body = collect(stream)
        .await
        .expect("discovered total must drive the remaining resume");

    assert_eq!(body.len(), BODY_LEN);
    assert!(body.iter().all(|byte| *byte == BODY_BYTE));
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_open_ended_stream_delivers_chunks() {
    let server =
        serve(|| async { respond(StatusCode::OK, &[], unsized_body(&[b"abc", b"def"])) }).await;

    let client = AppleNet::new(fast_options(0), pools(), CancelToken::never());
    let stream = client
        .stream(server.url(PROBE), None)
        .await
        .expect("stream");

    assert_eq!(&collect(stream).await.expect("body")[..], b"abcdef");
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_short_body_yields_before_premature_eof_under_flash() {
    const BODY_LEN: usize = 2 * 1024 * 1024;
    const FIRST_WRITE_LEN: usize = 1024 * 1024;

    let body = Bytes::from(
        (0..BODY_LEN)
            .map(|i| u8::try_from(i % 251).expect("modulo value fits in u8"))
            .collect::<Vec<_>>(),
    );
    let resume_offset = Arc::new(AtomicUsize::new(usize::MAX));
    let accept_encodings = Arc::new(Mutex::new(Vec::new()));
    let served_body = body.clone();
    let seen_resume = Arc::clone(&resume_offset);
    let seen_accept_encodings = Arc::clone(&accept_encodings);
    let server = serve(move |headers: HeaderMap| {
        let body = served_body.clone();
        let resume_offset = Arc::clone(&seen_resume);
        let accept_encodings = Arc::clone(&seen_accept_encodings);
        async move {
            accept_encodings.lock().push(
                request_header(&headers, "accept-encoding")
                    .unwrap_or_default()
                    .to_string(),
            );
            range_start(&headers).map_or_else(
                || {
                    truncated(
                        Response::builder()
                            .header("Content-Length", body.len())
                            .header("Accept-Ranges", "bytes"),
                        body.slice(..FIRST_WRITE_LEN),
                        Duration::from_millis(20),
                    )
                },
                |offset| {
                    resume_offset.store(offset, Ordering::SeqCst);
                    let content_range =
                        format!("bytes {}-{}/{}", offset, body.len() - 1, body.len());
                    respond(
                        StatusCode::PARTIAL_CONTENT,
                        &[("Content-Range", &content_range)],
                        body.slice(offset.min(body.len())..),
                    )
                },
            )
        }
    })
    .await;

    let client = AppleNet::new(fast_options(1), pools(), CancelToken::never());
    let stream = client
        .stream(server.url(PROBE), None)
        .await
        .expect("stream");
    let body_out = collect(stream).await.expect("body");

    let actual_resume = resume_offset.load(Ordering::SeqCst);
    assert!(
        (1..BODY_LEN).contains(&actual_resume),
        "resume must start after delivered partial bytes, got {actual_resume}"
    );
    assert_eq!(&body_out[..], &body[..]);
    let accept_encodings = accept_encodings.lock().clone();
    assert!(
        accept_encodings.len() >= 2,
        "stream must include its initial request and at least one resume"
    );
    assert!(
        accept_encodings.iter().all(|value| value == "identity"),
        "every initial and resumed stream request must use identity: {:?}",
        &*accept_encodings
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_stream_head_stall_times_out() {
    let server = serve(|| async { stalled_head() }).await;
    let url = server.url(PROBE);

    let client = AppleNet::new(stream_options(300), pools(), CancelToken::never());
    let result = client.stream(url, None).await;

    match result {
        Err(error) => assert!(matches!(error, NetError::Timeout)),
        Ok(_) => panic!("head stall must time out before stream opens"),
    }
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_stream_observes_cancellation() {
    let server = serve(|| async { stalled_head() }).await;
    let url = server.url(PROBE);

    let cancel = CancelToken::root();
    let client = AppleNet::new(stream_options(5000), pools(), cancel.clone());
    let cancel_task = cancel.clone();
    drop(spawn(async move {
        kithara_platform::time::sleep(Duration::from_millis(50)).await;
        cancel_task.cancel();
    }));
    let result = client.stream(url, None).await;

    match result {
        Err(error) => assert!(matches!(error, NetError::Cancelled)),
        Ok(_) => panic!("head wait must observe cancellation"),
    }
}
