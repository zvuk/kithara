#![forbid(unsafe_code)]

use std::{
    io::{Read, Seek, SeekFrom},
    time::Duration,
};

use axum::{Router, response::Response, routing::get};
use bytes::Bytes;
use kithara_assets::StoreOptions;
use kithara_file::{File, FileConfig};
use kithara_stream::Stream;
use rstest::{fixture, rstest};
use tempfile::TempDir;
use tokio::net::TcpListener;

// ==================== Test Server Fixtures ====================

async fn test_audio_endpoint() -> Response {
    let audio_data = Bytes::from_static(b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345");
    Response::builder()
        .status(200)
        .body(axum::body::Body::from(audio_data))
        .unwrap()
}

async fn test_large_endpoint() -> Response {
    let large_data = Bytes::from_static(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#$%^&*()");
    Response::builder()
        .status(200)
        .body(axum::body::Body::from(large_data))
        .unwrap()
}

fn test_app() -> Router {
    Router::new()
        .route("/audio.mp3", get(test_audio_endpoint))
        .route("/large.bin", get(test_large_endpoint))
}

async fn run_test_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = test_app();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    format!("http://127.0.0.1:{}", addr.port())
}

// ==================== Fixtures ====================

#[fixture]
fn temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

#[fixture]
async fn test_server() -> String {
    run_test_server().await
}

// ==================== Stream<File> Seek Tests ====================

#[rstest]
#[case(0, b"ID3\x04\x00")]
#[case(5, b"\x00\x00\x00\x00T")]
#[case(10, b"estAu")]
#[case(22, b"12345")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(10))]
async fn stream_file_seek_start_reads_correct_bytes(
    #[future] test_server: String,
    temp_dir: TempDir,
    #[case] seek_pos: u64,
    #[case] expected: &[u8],
) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();

    let config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let mut stream = Stream::<File>::new(config).await.unwrap();

    let expected_len = expected.len();
    let expected_vec = expected.to_vec();

    let result = tokio::task::spawn_blocking(move || {
        let pos = stream.seek(SeekFrom::Start(seek_pos)).unwrap();
        assert_eq!(pos, seek_pos);

        let mut buf = vec![0u8; expected_len];
        let n = stream.read(&mut buf).unwrap();
        (n, buf)
    })
    .await
    .unwrap();

    assert_eq!(result.0, expected_len);
    assert_eq!(&result.1[..result.0], &expected_vec[..]);
}

#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_file_seek_current_works(#[future] test_server: String, temp_dir: TempDir) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();

    let config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let mut stream = Stream::<File>::new(config).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Read first 5 bytes
        let mut buf = [0u8; 5];
        stream.read(&mut buf).unwrap();
        assert_eq!(&buf, b"ID3\x04\x00");

        // Seek forward 5 bytes (position = 5 + 5 = 10)
        let pos = stream.seek(SeekFrom::Current(5)).unwrap();
        assert_eq!(pos, 10);

        // Read from position 10
        let mut buf = [0u8; 4];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"estA");
    })
    .await
    .unwrap();
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(10))]
async fn stream_file_seek_end_works(#[future] test_server: String, temp_dir: TempDir) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();

    let config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let mut stream = Stream::<File>::new(config).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Seek from end (-5 bytes)
        let pos = stream.seek(SeekFrom::End(-5)).unwrap();
        // Test data: b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345" = 27 bytes
        assert_eq!(pos, 22);

        // Read last 5 bytes
        let mut buf = [0u8; 5];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"12345");
    })
    .await
    .unwrap();
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(10))]
async fn stream_file_seek_past_eof_fails(#[future] test_server: String, temp_dir: TempDir) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();

    let config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let mut stream = Stream::<File>::new(config).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Attempt to seek past EOF
        let result = stream.seek(SeekFrom::Start(1000));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    })
    .await
    .unwrap();
}

#[rstest]
#[timeout(Duration::from_secs(10))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_file_multiple_seeks_work(#[future] test_server: String, temp_dir: TempDir) {
    let server_url = test_server.await;
    let url: url::Url = format!("{}/audio.mp3", server_url).parse().unwrap();

    let config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let mut stream = Stream::<File>::new(config).await.unwrap();

    tokio::task::spawn_blocking(move || {
        // Read from start
        let mut buf = [0u8; 3];
        stream.read(&mut buf).unwrap();
        assert_eq!(&buf, b"ID3");

        // Seek to middle
        stream.seek(SeekFrom::Start(13)).unwrap();
        let mut buf = [0u8; 5];
        stream.read(&mut buf).unwrap();
        assert_eq!(&buf, b"Audio");

        // Seek back to start
        stream.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = [0u8; 3];
        stream.read(&mut buf).unwrap();
        assert_eq!(&buf, b"ID3");

        // Seek to end
        let pos = stream.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(pos, 27);
    })
    .await
    .unwrap();
}
