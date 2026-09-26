use kithara::platform::time::Duration;
use kithara_integration_tests::{HlsFixtureBuilder, TestServerHelper, kithara};
use kithara_test_fixtures::SignalAsset;
use reqwest::Client;

#[kithara::test(tokio, timeout(Duration::from_secs(5)), hang_timeout_secs(1))]
async fn test_test_server_helper_serves_audio_fixture_urls(
    #[future(awt)] server: TestServerHelper,
) {
    let wav_url = server.signal(SignalAsset::WAV_SAW_1S);
    let mp3_url = server.signal(SignalAsset::MP3_TRACK_SINE440_187S);

    assert!(wav_url.as_str().starts_with("http://127.0.0.1:"));
    assert!(mp3_url.as_str().starts_with("http://127.0.0.1:"));
    assert!(wav_url.path().starts_with("/signal/"));
    assert!(wav_url.path().ends_with(".wav"));
    assert!(mp3_url.path().starts_with("/signal/"));
    assert!(mp3_url.path().ends_with(".mp3"));
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(5)), hang_timeout_secs(1))]
#[case("wav", "audio/wav", "WAV file")]
#[case("mp3", "audio/mpeg", "MP3 file")]
async fn test_test_server_helper_serves_format(
    #[case] format: &str,
    #[case] content_type: &str,
    #[case] desc: &str,
    #[future(awt)] server: TestServerHelper,
) {
    let client = Client::new();

    let url = match format {
        "wav" => server.signal(SignalAsset::WAV_SAW_1S),
        "mp3" => server.signal(SignalAsset::MP3_TRACK_SINE440_187S),
        _ => panic!("Unknown format: {}", format),
    };

    let response = client
        .get(url)
        .send()
        .await
        .unwrap_or_else(|e| panic!("Failed to fetch {}: {}", desc, e));

    assert_eq!(response.status(), 200, "{}: status", desc);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        content_type,
        "{}: content-type",
        desc
    );

    let content_length: usize = response
        .headers()
        .get("content-length")
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();

    assert!(content_length > 0, "{}: content length should be > 0", desc);
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(5)), hang_timeout_secs(1))]
async fn test_create_packaged_hls_returns_stable_typed_urls(
    #[future(awt)] server: TestServerHelper,
) {
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(4)
                .segment_duration_secs(1.0)
                .packaged_audio_aac_lc(44_100, 2),
        )
        .await
        .expect("create HLS fixture");

    let master_url = created.master_url();
    let media_url = created.media_url(0);
    let init_url = created.init_url(0);
    let segment_url = created.segment_url(0, 0);
    let token = created.token().to_string();

    assert!(master_url.path().contains(&token));
    assert!(media_url.path().contains(&token));
    assert!(init_url.path().contains(&token));
    assert!(segment_url.path().contains(&token));

    let client = Client::new();
    let master = client.get(master_url).send().await.unwrap();
    let init = client.get(init_url).send().await.unwrap();
    let segment = client.get(segment_url).send().await.unwrap();

    assert_eq!(master.status(), 200);
    assert_eq!(
        master.headers().get("content-type").unwrap(),
        "application/vnd.apple.mpegurl"
    );
    assert_eq!(init.status(), 200);
    assert_eq!(init.headers().get("content-type").unwrap(), "audio/mp4");
    assert_eq!(segment.status(), 200);
    assert_eq!(segment.headers().get("content-type").unwrap(), "audio/mp4");
    assert!(!init.bytes().await.unwrap().is_empty());
    assert!(!segment.bytes().await.unwrap().is_empty());
}

#[kithara::fixture]
async fn server() -> TestServerHelper {
    TestServerHelper::new().await
}
