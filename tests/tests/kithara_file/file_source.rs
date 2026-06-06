#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::Arc,
};

use kithara::{
    assets::StoreOptions,
    file::{File, FileConfig},
    stream::{AudioCodec, ContainerFormat, Stream},
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir, temp_dir,
};
use kithara_platform::{time::Duration, tokio::task::spawn_blocking};
use url::Url;

struct Consts;
impl Consts {
    const AUDIO_DATA: &'static [u8] = b"ID3\x04\x00\x00\x00\x00\x00TestAudioData12345";
}

/// Register the 27-byte audio fixture with optional MIME, served over
/// HTTP range requests, and return its URL.
fn audio_behavior(helper: &TestServerHelper, content_type: Option<&'static str>) -> Url {
    let handle = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(Consts::AUDIO_DATA.to_vec()),
            content_type,
        },
        delivery: Delivery::Range,
    });
    handle.url()
}

#[kithara::test(
    flash(false),
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case(0, b"ID3\x04\x00")]
#[case(5, b"\x00\x00\x00\x00T")]
#[case(10, b"estAu")]
#[case(22, b"12345")]
async fn stream_file_seek_start_reads_correct_bytes(
    temp_dir: TestTempDir,
    #[case] seek_pos: u64,
    #[case] expected: &[u8],
) {
    let helper = TestServerHelper::new().await;
    let url = audio_behavior(&helper, None);

    let config = FileConfig::for_src(url.into())
        .store(StoreOptions::new(temp_dir.path()))
        .build();
    let mut stream = Stream::<File>::new(config).await.unwrap();

    let expected_len = expected.len();
    let expected_vec = expected.to_vec();

    let result = spawn_blocking(move || {
        let mut primer = [0u8; 1];
        let _ = stream.read(&mut primer).unwrap();

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

#[kithara::test(
    flash(false),
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::current_after_read(Some((5, b"ID3\x04\x00")), SeekFrom::Current(5), 10, b"estA")]
#[case::end_from_fresh(None, SeekFrom::End(-5), 22, b"12345")]
async fn stream_file_seek_reads_expected_bytes(
    temp_dir: TestTempDir,
    #[case] initial_read: Option<(usize, &'static [u8])>,
    #[case] seek_from: SeekFrom,
    #[case] expected_pos: u64,
    #[case] expected: &'static [u8],
) {
    let helper = TestServerHelper::new().await;
    let url = audio_behavior(&helper, None);

    let config = FileConfig::for_src(url.into())
        .store(StoreOptions::new(temp_dir.path()))
        .build();
    let mut stream = Stream::<File>::new(config).await.unwrap();

    spawn_blocking(move || {
        if let Some((len, prefix)) = initial_read {
            let mut buf = vec![0u8; len];
            let n = stream.read(&mut buf).unwrap();
            assert_eq!(n, len);
            assert_eq!(&buf[..n], prefix);
        }

        let pos = stream.seek(seek_from).unwrap();
        assert_eq!(pos, expected_pos);

        let mut buf = vec![0u8; expected.len()];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, expected.len());
        assert_eq!(&buf[..n], expected);
    })
    .await
    .unwrap();
}

/// Regression: standalone HTTP file sources used to discard the
/// container hint from `Content-Type`, yielding `MediaInfo { codec:
/// Some(_), container: None }`. With Apple-only desktop builds (no
/// Symphonia fallback) `apple_standalone_supports(codec, None)` is
/// false → `DecoderFactory` returns `UnsupportedCodec` and the track
/// fails to load. Production stream URL: `cdn-edge.zvq.me/track/streamhq?id=…`
/// served as `audio/mpeg`.
#[kithara::test(
    flash(false),
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case("audio/mpeg", AudioCodec::Mp3, ContainerFormat::MpegAudio)]
#[case("audio/flac", AudioCodec::Flac, ContainerFormat::Flac)]
#[case("audio/wav", AudioCodec::Pcm, ContainerFormat::Wav)]
async fn stream_media_info_carries_container_from_content_type(
    temp_dir: TestTempDir,
    #[case] mime: &'static str,
    #[case] expected_codec: AudioCodec,
    #[case] expected_container: ContainerFormat,
) {
    let helper = TestServerHelper::new().await;
    let url = audio_behavior(&helper, Some(mime));

    let config = FileConfig::for_src(url.into())
        .store(StoreOptions::new(temp_dir.path()))
        .build();
    let mut stream = Stream::<File>::new(config).await.unwrap();

    let info = spawn_blocking(move || {
        let mut primer = [0u8; 1];
        let _ = stream.read(&mut primer).unwrap();
        stream.media_info()
    })
    .await
    .unwrap();

    let info = info.expect("media_info must be available once Content-Type arrived");
    assert_eq!(
        info.codec,
        Some(expected_codec),
        "{mime}: codec lost; got {:?}",
        info.codec
    );
    assert_eq!(
        info.container,
        Some(expected_container),
        "{mime}: container dropped on the floor (regression — Apple-only build will fail to dispatch); got {:?}",
        info.container
    );
}

#[kithara::test(
    flash(false),
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn stream_file_seek_past_eof_fails(temp_dir: TestTempDir) {
    let helper = TestServerHelper::new().await;
    let url = audio_behavior(&helper, None);

    let config = FileConfig::for_src(url.into())
        .store(StoreOptions::new(temp_dir.path()))
        .build();
    let mut stream = Stream::<File>::new(config).await.unwrap();

    spawn_blocking(move || {
        let result = stream.seek(SeekFrom::Start(1000));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    })
    .await
    .unwrap();
}

#[kithara::test(
    flash(false),
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn stream_file_multiple_seeks_work(temp_dir: TestTempDir) {
    let helper = TestServerHelper::new().await;
    let url = audio_behavior(&helper, None);

    let config = FileConfig::for_src(url.into())
        .store(StoreOptions::new(temp_dir.path()))
        .build();
    let mut stream = Stream::<File>::new(config).await.unwrap();

    spawn_blocking(move || {
        let mut buf = [0u8; 3];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf, b"ID3");

        stream.seek(SeekFrom::Start(13)).unwrap();
        let mut buf = [0u8; 5];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"Audio");

        stream.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = [0u8; 3];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf, b"ID3");

        let pos = stream.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(pos, 27);
    })
    .await
    .unwrap();
}
