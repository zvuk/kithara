mod kithara {
    pub(crate) use kithara_test_macros::test;
}

use std::{
    future::Future,
    net::SocketAddr,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

use bytes::Bytes;
use futures::StreamExt;
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
    time::Duration,
    tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::spawn,
    },
};
use url::Url;

use super::client::AppleNet;
use crate::{
    error::NetError,
    types::{Compression, Headers, NetOptions, RangeSpec, RetryPolicy},
};

const PLAYLIST: &[u8] = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\na.m3u8\n";
const GZIP_PLAYLIST: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x53, 0x76, 0x8d, 0x08, 0xf1, 0x35,
    0x0e, 0xe5, 0x52, 0x06, 0xd2, 0xba, 0x11, 0xba, 0xc1, 0x21, 0x41, 0xae, 0x8e, 0xbe, 0xba, 0x9e,
    0x7e, 0x6e, 0x56, 0x4e, 0x8e, 0x7e, 0x2e, 0xe1, 0x9e, 0x2e, 0x21, 0x1e, 0xb6, 0x86, 0x5c, 0x89,
    0x7a, 0xb9, 0xc6, 0xa5, 0x16, 0x5c, 0x00, 0xf1, 0x51, 0x3e, 0xd3, 0x2d, 0x00, 0x00, 0x00,
];

async fn spawn_server<F, Fut>(handler: F) -> Url
where
    F: Fn(TcpStream, String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    let handler = Arc::new(handler);
    spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let handler = Arc::clone(&handler);
            spawn(async move {
                let request = read_request(&mut socket).await;
                handler(socket, request).await;
            });
        }
    });
    Url::parse(&format!("http://{addr}/probe")).expect("url")
}

async fn read_request(socket: &mut TcpStream) -> String {
    let mut buf = [0; 4096];
    let n = socket.read(&mut buf).await.expect("read request");
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

async fn write_response(
    mut socket: TcpStream,
    status: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (key, value) in headers {
        head.push_str(key);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    socket.write_all(head.as_bytes()).await.expect("write head");
    socket.write_all(body).await.expect("write body");
}

async fn write_raw(mut socket: TcpStream, response: &'static [u8]) {
    socket.write_all(response).await.expect("write response");
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

fn range_start(request: &str) -> Option<usize> {
    request
        .lines()
        .find_map(|line| line.strip_prefix("Range: bytes="))
        .and_then(|range| range.split_once('-').map(|(start, _)| start))
        .and_then(|start| start.parse().ok())
}

fn request_header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn overriding_accept_encoding() -> Headers {
    let mut headers = Headers::new();
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
    let url = spawn_server(move |socket, _request| {
        let seen = Arc::clone(&seen);
        async move {
            let attempt = seen.fetch_add(1, Ordering::SeqCst);
            if attempt < 2 {
                write_response(socket, "503 Service Unavailable", &[], b"busy").await;
            } else {
                write_response(socket, "200 OK", &[], b"ok").await;
            }
        }
    })
    .await;

    let client = AppleNet::new(fast_options(3), CancelToken::never());
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
    let url = spawn_server(move |socket, request| {
        let seen = Arc::clone(&handler_seen);
        async move {
            let accept_encoding = request_header(&request, "accept-encoding")
                .unwrap_or_default()
                .to_string();
            seen.lock().push(accept_encoding);
            if request.starts_with("HEAD ") {
                write_raw(
                    socket,
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n",
                )
                .await;
            } else {
                write_response(socket, "200 OK", &[], b"ok").await;
            }
        }
    })
    .await;

    let options = NetOptions::builder()
        .compression(Compression::GZIP | Compression::DEFLATE)
        .build();
    let client = AppleNet::new(options, CancelToken::never());

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
    let url = spawn_server(|socket, request| async move {
        assert_eq!(request_header(&request, "accept-encoding"), Some("gzip"));
        write_response(
            socket,
            "200 OK",
            &[("Content-Encoding", "gzip")],
            GZIP_PLAYLIST,
        )
        .await;
    })
    .await;
    let options = NetOptions::builder().compression(Compression::GZIP).build();
    let client = AppleNet::new(options, CancelToken::never());

    let body = client
        .get_bytes(url, None)
        .await
        .expect("Foundation auto-decodes configured gzip");

    assert_eq!(&body[..], PLAYLIST);
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_identity_stream_rejects_nonidentity_content_encoding() {
    let url = spawn_server(|socket, _request| async move {
        write_response(
            socket,
            "200 OK",
            &[("CoNtEnT-EnCoDiNg", "gzip")],
            GZIP_PLAYLIST,
        )
        .await;
    })
    .await;
    let client = AppleNet::new(fast_options(0), CancelToken::never());

    let Err(error) = client.stream(url.clone(), None).await else {
        panic!("encoded bytes must not reach an identity stream");
    };

    assert!(
        matches!(error, NetError::Decode(detail) if detail.contains(url.as_str()) && detail.contains("gzip"))
    );
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_head_backfills_content_length_from_content_range() {
    let url = spawn_server(|socket, _request| async move {
            write_raw(
                socket,
                b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/1234\r\nConnection: close\r\n\r\n",
            )
            .await;
        })
        .await;

    let client = AppleNet::new(fast_options(0), CancelToken::never());
    let headers = client.head(url, None).await.expect("head");

    assert_eq!(headers.get("content-length"), Some("1234"));
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_open_ended_stream_delivers_chunks() {
    let url = spawn_server(|socket, _request| async move {
            write_raw(
                socket,
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n",
            )
            .await;
        })
        .await;

    let client = AppleNet::new(fast_options(0), CancelToken::never());
    let stream = client.stream(url, None).await.expect("stream");

    assert_eq!(&collect(stream).await.expect("body")[..], b"abcdef");
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_short_body_yields_before_premature_eof_under_flash() {
    const BODY_LEN: usize = 2 * 1024 * 1024;
    const FIRST_WRITE_LEN: usize = 1024 * 1024;

    let body = Arc::new(
        (0..BODY_LEN)
            .map(|i| u8::try_from(i % 251).expect("modulo value fits in u8"))
            .collect::<Vec<_>>(),
    );
    let resume_offset = Arc::new(AtomicUsize::new(usize::MAX));
    let accept_encodings = Arc::new(Mutex::new(Vec::new()));
    let served_body = Arc::clone(&body);
    let seen_resume = Arc::clone(&resume_offset);
    let seen_accept_encodings = Arc::clone(&accept_encodings);
    let url = spawn_server(move |mut socket, request| {
        let body = Arc::clone(&served_body);
        let resume_offset = Arc::clone(&seen_resume);
        let accept_encodings = Arc::clone(&seen_accept_encodings);
        async move {
            accept_encodings.lock().push(
                request_header(&request, "accept-encoding")
                    .unwrap_or_default()
                    .to_string(),
            );
            if let Some(offset) = range_start(&request) {
                resume_offset.store(offset, Ordering::SeqCst);
                let tail = &body[offset.min(body.len())..];
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                    tail.len(),
                    offset,
                    body.len() - 1,
                    body.len()
                );
                socket
                    .write_all(head.as_bytes())
                    .await
                    .expect("write range head");
                socket.write_all(tail).await.expect("write range body");
            } else {
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket.write_all(head.as_bytes()).await.expect("write head");
                socket
                    .write_all(&body[..FIRST_WRITE_LEN])
                    .await
                    .expect("write short body");
                kithara_platform::time::sleep(Duration::from_millis(20)).await;
            }
        }
    })
    .await;

    let client = AppleNet::new(fast_options(1), CancelToken::never());
    let stream = client.stream(url, None).await.expect("stream");
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
    let url = spawn_server(|mut socket, _request| async move {
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .expect("write head");
        kithara_platform::time::sleep(Duration::from_secs(2)).await;
    })
    .await;

    let client = AppleNet::new(stream_options(300), CancelToken::never());
    let result = client.stream(url, None).await;

    match result {
        Err(error) => assert!(matches!(error, NetError::Timeout)),
        Ok(_) => panic!("head stall must time out before stream opens"),
    }
}

#[kithara::test(tokio, timeout(Duration::from_secs(5)))]
async fn apple_stream_observes_cancellation() {
    let url = spawn_server(|mut socket, _request| async move {
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .expect("write head");
        kithara_platform::time::sleep(Duration::from_secs(3)).await;
    })
    .await;

    let cancel = CancelToken::root();
    let client = AppleNet::new(stream_options(5000), cancel.clone());
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
