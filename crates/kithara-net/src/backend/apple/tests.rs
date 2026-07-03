mod kithara {
    pub(crate) use kithara_test_macros::test;
}

use std::{
    future::Future,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicUsize, Ordering},
    },
};

use bytes::Bytes;
use futures::StreamExt;
use kithara_platform::{
    CancelToken,
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
    types::{NetOptions, RetryPolicy},
};

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
        .total_timeout(Duration::from_secs(5))
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
        .total_timeout(Duration::from_secs(5))
        .build()
}

fn range_start(request: &str) -> Option<usize> {
    request
        .lines()
        .find_map(|line| line.strip_prefix("Range: bytes="))
        .and_then(|range| range.split_once('-').map(|(start, _)| start))
        .and_then(|start| start.parse().ok())
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
    let served_body = Arc::clone(&body);
    let seen_resume = Arc::clone(&resume_offset);
    let url = spawn_server(move |mut socket, request| {
        let body = Arc::clone(&served_body);
        let resume_offset = Arc::clone(&seen_resume);
        async move {
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
