//! Integration tests for audio fixtures.
//!
//! Tests that verify the audio fixtures work correctly and can be used
//! by decode tests without external network access.

mod fixture;

#[tokio::test]
async fn test_audio_test_server_starts() {
    // Test that the server can start and serve requests
    let server = fixture::AudioTestServer::new().await;

    // Verify we can get URLs
    let wav_url = server.wav_url();
    let mp3_url = server.mp3_url();

    assert!(wav_url.as_str().starts_with("http://127.0.0.1:"));
    assert!(mp3_url.as_str().starts_with("http://127.0.0.1:"));
    assert!(wav_url.as_str().ends_with("/silence.wav"));
    assert!(mp3_url.as_str().ends_with("/test.mp3"));
}

#[tokio::test]
async fn test_audio_test_server_serves_wav() {
    let server = fixture::AudioTestServer::new().await;
    let client = reqwest::Client::new();

    let response = client
        .get(server.wav_url())
        .send()
        .await
        .expect("Failed to fetch WAV");

    assert_eq!(response.status(), 200);
    assert_eq!(response.headers().get("content-type").unwrap(), "audio/wav");

    let content_length: usize = response
        .headers()
        .get("content-length")
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();

    assert!(content_length > 0);
    assert_eq!(server.request_count("/silence.wav"), 1);
}

#[tokio::test]
async fn test_audio_test_server_serves_mp3() {
    let server = fixture::AudioTestServer::new().await;
    let client = reqwest::Client::new();

    let response = client
        .get(server.mp3_url())
        .send()
        .await
        .expect("Failed to fetch MP3");

    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "audio/mpeg"
    );

    let content_length: usize = response
        .headers()
        .get("content-length")
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();

    assert!(content_length > 0);
    assert_eq!(server.request_count("/test.mp3"), 1);
}

#[test]
fn test_embedded_audio_contains_data() {
    let audio = fixture::EmbeddedAudio::get();

    // Verify WAV data exists
    let wav_data = audio.wav();
    assert!(!wav_data.is_empty());

    // Verify MP3 data exists
    let mp3_data = audio.mp3();
    assert!(!mp3_data.is_empty());

    // MP3 should be larger than WAV (our test MP3 is 2.9MB)
    assert!(mp3_data.len() > wav_data.len());
}

// Note: More comprehensive decode tests will be added when the actual
// decode functionality is implemented. These tests just verify the
// fixture infrastructure works correctly.
