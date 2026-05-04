//! Integration tests for audio fixtures.
//!
//! Tests that verify the audio fixtures work correctly and can be used
//! by decode tests without external network access.

use std::{fs, io::Cursor, process::Command};

use kithara::{
    decode::{DecoderConfig, DecoderFactory},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_integration_tests::audio_fixture::EmbeddedAudio;
use kithara_platform::time::Duration;
use kithara_test_utils::{
    HlsFixtureBuilder, PackagedTestServer, SignalDirection, SignalFormat, SignalSpec,
    SignalSpecLength, TestServerHelper, detect_direction, fixture_protocol::PackagedSignal,
};
use reqwest::Client;

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_test_server_helper_serves_audio_fixture_urls() {
    let server = TestServerHelper::new().await;

    let wav_url = server.sawtooth(&wav_spec()).await;
    let mp3_url = server.asset("test.mp3");

    assert!(wav_url.as_str().starts_with("http://127.0.0.1:"));
    assert!(mp3_url.as_str().starts_with("http://127.0.0.1:"));
    assert!(wav_url.path().starts_with("/signal/sawtooth/"));
    assert!(wav_url.path().ends_with(".wav"));
    assert!(mp3_url.path().ends_with("test.mp3"));
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case("wav", "audio/wav", "WAV file")]
#[case("mp3", "audio/mpeg", "MP3 file")]
async fn test_test_server_helper_serves_format(
    #[case] format: &str,
    #[case] content_type: &str,
    #[case] desc: &str,
) {
    let server = TestServerHelper::new().await;
    let client = Client::new();

    let url = match format {
        "wav" => server.sawtooth(&wav_spec()).await,
        "mp3" => server.asset("test.mp3"),
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

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case(SignalFormat::Mp3, "mp3", "audio/mpeg")]
#[case(SignalFormat::Flac, "flac", "audio/flac")]
#[case(SignalFormat::Aac, "aac", "audio/aac")]
#[case(SignalFormat::M4a, "m4a", "audio/mp4")]
async fn test_signal_server_encoded_formats_are_decodable(
    #[case] format: SignalFormat,
    #[case] ext: &str,
    #[case] content_type: &str,
) {
    let server = TestServerHelper::new().await;
    let client = Client::new();
    let spec = SignalSpec {
        sample_rate: 44_100,
        channels: 2,
        length: SignalSpecLength::Seconds(1.0),
        format,
    };

    let response = client
        .get(server.sawtooth(&spec).await)
        .send()
        .await
        .unwrap_or_else(|error| panic!("Failed to fetch /signal encoded fixture: {error}"));

    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        content_type
    );

    let bytes = response.bytes().await.unwrap();
    assert!(!bytes.is_empty());

    let mut decoder = DecoderFactory::create_with_probe(
        Cursor::new(bytes.to_vec()),
        Some(ext),
        DecoderConfig::default(),
    )
    .unwrap();

    let chunk = decoder.next_chunk().unwrap().into_chunk().unwrap();
    assert!(!chunk.pcm.is_empty());
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case(SignalFormat::Aac, "aac", "audio/aac")]
#[case(SignalFormat::Flac, "flac", "audio/flac")]
async fn test_signal_server_aac_and_flac_roundtrip_produce_expected_pcm(
    #[case] format: SignalFormat,
    #[case] ext: &str,
    #[case] content_type: &str,
) {
    let server = TestServerHelper::new().await;
    let client = Client::new();
    let spec = SignalSpec {
        sample_rate: 44_100,
        channels: 2,
        length: SignalSpecLength::Seconds(1.0),
        format,
    };

    let response = client
        .get(server.sawtooth(&spec).await)
        .send()
        .await
        .unwrap_or_else(|error| panic!("Failed to fetch /signal round-trip fixture: {error}"));

    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        content_type
    );

    let bytes = response.bytes().await.unwrap();
    let mut decoder = DecoderFactory::create_with_probe(
        Cursor::new(bytes.to_vec()),
        Some(ext),
        DecoderConfig::default(),
    )
    .unwrap_or_else(|error| panic!("probe {format:?} decode failed: {error}"));

    let pcm_spec = decoder.spec();
    assert_eq!(pcm_spec.sample_rate, 44_100);
    assert_eq!(pcm_spec.channels, 2);

    let mut total_frames = 0usize;
    let mut ascending_chunks = 0usize;
    for chunk_idx in 0..8 {
        let outcome = decoder.next_chunk().unwrap_or_else(|error| {
            panic!("decode chunk {chunk_idx} failed for {format:?}: {error}")
        });
        let Some(chunk) = outcome.into_chunk() else {
            break;
        };
        assert_eq!(chunk.spec().sample_rate, 44_100);
        assert_eq!(chunk.spec().channels, 2);
        assert_valid_pcm_samples(&chunk.pcm, format!("{format:?} chunk {chunk_idx}").as_str());
        total_frames += chunk.frames();
        if detect_direction(&chunk.pcm, chunk.spec().channels as usize)
            == SignalDirection::Ascending
        {
            ascending_chunks += 1;
        }
    }

    assert!(
        total_frames >= 4096,
        "{format:?} should decode a meaningful amount of PCM, got {total_frames} frames"
    );
    assert!(
        ascending_chunks > 0,
        "{format:?} round-trip should preserve the sawtooth direction"
    );
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_create_packaged_hls_returns_stable_typed_urls() {
    let server = TestServerHelper::new().await;
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

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn test_packaged_test_server_serves_audio_mp4_resources() {
    let server = PackagedTestServer::new().await;
    let client = Client::new();

    let master = client.get(server.url("/master.m3u8")).send().await.unwrap();
    let media = client.get(server.url("/v0.m3u8")).send().await.unwrap();
    let init = client.get(server.url("/init/v0.mp4")).send().await.unwrap();
    let segment = client
        .get(server.url("/seg/v0_0.m4s"))
        .send()
        .await
        .unwrap();

    assert_eq!(master.status(), 200);
    assert_eq!(
        master.headers().get("content-type").unwrap(),
        "application/vnd.apple.mpegurl"
    );
    assert_eq!(media.status(), 200);
    assert_eq!(
        media.headers().get("content-type").unwrap(),
        "application/vnd.apple.mpegurl"
    );
    assert_eq!(init.status(), 200);
    assert_eq!(init.headers().get("content-type").unwrap(), "audio/mp4");
    assert_eq!(segment.status(), 200);
    assert_eq!(segment.headers().get("content-type").unwrap(), "audio/mp4");
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::aac("aac", AudioCodec::AacLc)]
#[case::flac("flac", AudioCodec::Flac)]
async fn test_packaged_hls_aac_and_flac_roundtrip_decode_descending_saw(
    #[case] label: &str,
    #[case] codec: AudioCodec,
) {
    let server = TestServerHelper::new().await;
    let builder = match codec {
        AudioCodec::AacLc => HlsFixtureBuilder::new()
            .variant_count(1)
            .segments_per_variant(4)
            .segment_duration_secs(1.0)
            .packaged_audio_signal_aac_lc(44_100, 2, PackagedSignal::SawtoothDescending),
        AudioCodec::Flac => HlsFixtureBuilder::new()
            .variant_count(1)
            .segments_per_variant(4)
            .segment_duration_secs(1.0)
            .packaged_audio_signal_flac(44_100, 2, PackagedSignal::SawtoothDescending),
        other => panic!("unsupported packaged codec case: {other:?}"),
    };
    let created = server
        .create_hls(builder)
        .await
        .unwrap_or_else(|error| panic!("create packaged {label} fixture: {error}"));

    let client = Client::new();
    let init = client
        .get(created.init_url(0))
        .send()
        .await
        .unwrap_or_else(|error| panic!("fetch packaged {label} init: {error}"))
        .bytes()
        .await
        .unwrap();
    let media_playlist = client
        .get(created.media_url(0))
        .send()
        .await
        .unwrap_or_else(|error| panic!("fetch packaged {label} media playlist: {error}"))
        .text()
        .await
        .unwrap();
    let segment_paths = media_playlist
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>();
    assert!(
        !segment_paths.is_empty(),
        "packaged {label} media playlist must list at least one segment"
    );
    assert!(
        segment_paths.len() > 1,
        "packaged {label} media playlist should expose multiple segments, got {segment_paths:?}"
    );

    let mut segment_lengths = Vec::with_capacity(segment_paths.len());
    let mut mp4_bytes = Vec::with_capacity(init.len() + 16_384 * segment_paths.len());
    mp4_bytes.extend_from_slice(&init);
    for segment_path in segment_paths {
        let segment_url = created
            .media_url(0)
            .join(segment_path)
            .unwrap_or_else(|error| {
                panic!("join packaged {label} segment url {segment_path}: {error}")
            });
        let segment_response = client
            .get(segment_url)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("fetch packaged {label} segment {segment_path}: {error}")
            });
        assert_eq!(
            segment_response.status(),
            200,
            "packaged {label} segment {segment_path} should return 200"
        );
        let segment = segment_response.bytes().await.unwrap();
        assert!(
            !segment.is_empty(),
            "packaged {label} segment {segment_path} must not be empty"
        );
        segment_lengths.push((segment_path.to_string(), segment.len()));
        mp4_bytes.extend_from_slice(&segment);
    }

    let samples = decode_fragment_with_ffmpeg(&mp4_bytes, &format!("packaged {label}"));
    assert_valid_pcm_samples(&samples, &format!("packaged {label} decoded PCM"));

    assert!(
        samples.len() >= 4096,
        "packaged {label} fragment should decode a meaningful amount of PCM, got {} samples",
        samples.len()
    );
    assert!(
        contains_direction_window(&samples, 2, SignalDirection::Descending),
        "packaged {label} fragment should preserve descending sawtooth PCM after round-trip"
    );
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(10)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::aac("aac", AudioCodec::AacLc)]
#[case::flac("flac", AudioCodec::Flac)]
async fn test_packaged_hls_concat_bytes_work_with_decoder_factory_direct_fmp4(
    #[case] label: &str,
    #[case] codec: AudioCodec,
) {
    let server = TestServerHelper::new().await;
    let builder = match codec {
        AudioCodec::AacLc => HlsFixtureBuilder::new()
            .variant_count(1)
            .segments_per_variant(8)
            .segment_duration_secs(0.5)
            .packaged_audio_aac_lc(44_100, 2),
        AudioCodec::Flac => HlsFixtureBuilder::new()
            .variant_count(1)
            .segments_per_variant(8)
            .segment_duration_secs(0.5)
            .packaged_audio_flac(44_100, 2),
        other => panic!("unsupported packaged codec case: {other:?}"),
    };
    let created = server
        .create_hls(builder)
        .await
        .unwrap_or_else(|error| panic!("create packaged {label} fixture: {error}"));

    let client = Client::new();
    let init = client
        .get(created.init_url(0))
        .send()
        .await
        .unwrap_or_else(|error| panic!("fetch packaged {label} init: {error}"))
        .bytes()
        .await
        .unwrap();
    let segment = client
        .get(created.segment_url(0, 0))
        .send()
        .await
        .unwrap_or_else(|error| panic!("fetch packaged {label} segment: {error}"))
        .bytes()
        .await
        .unwrap();

    let mut mp4_bytes = Vec::with_capacity(init.len() + segment.len());
    mp4_bytes.extend_from_slice(&init);
    mp4_bytes.extend_from_slice(&segment);

    let media_info = MediaInfo::new(Some(codec), Some(ContainerFormat::Fmp4))
        .with_sample_rate(44_100)
        .with_channels(2);
    let box_tags = scan_top_level_box_tags(&mp4_bytes);
    let box_summaries = scan_top_level_box_summaries(&mp4_bytes);
    assert!(
        box_tags.starts_with(&["ftyp".to_string(), "moov".to_string()]),
        "packaged {label} bytes must start with ftyp/moov, got {box_summaries:?}"
    );
    assert!(
        box_tags.iter().any(|tag| tag == "moof"),
        "packaged {label} bytes must contain moof, got {box_summaries:?}"
    );
    assert!(
        box_tags.iter().any(|tag| tag == "mdat"),
        "packaged {label} bytes must contain mdat, got {box_summaries:?}"
    );
    let mut direct_decoder = DecoderFactory::create_from_media_info(
        Cursor::new(mp4_bytes.clone()),
        &media_info,
        &DecoderConfig::default(),
    )
    .unwrap_or_else(|error| panic!("create decoder from packaged {label} fmp4: {error}"));

    let direct_chunk = direct_decoder
        .next_chunk()
        .unwrap_or_else(|error| panic!("decode first direct chunk for packaged {label}: {error}"));
    let total_len = mp4_bytes.len();
    let mut probe_decoder = DecoderFactory::create_with_probe(
        Cursor::new(mp4_bytes.clone()),
        Some("m4a"),
        DecoderConfig::default(),
    )
    .unwrap_or_else(|error| panic!("probe packaged {label} fmp4 decode failed: {error}"));
    let probe_chunk = probe_decoder
        .next_chunk()
        .unwrap_or_else(|error| panic!("decode first probe chunk for packaged {label}: {error}"));

    let chunk = direct_chunk.into_chunk().unwrap_or_else(|| {
        if probe_chunk.is_chunk() {
            panic!(
                "packaged {label} direct fmp4 decoder returned EOF, but probe-based decoder produced PCM; total_len={}, boxes={box_summaries:?}",
                total_len
            );
        }
        panic!(
            "packaged {label} decoder returned EOF on concat init+segment in both direct and probe modes; total_len={}, boxes={box_summaries:?}",
            total_len
        );
    });

    assert!(
        !chunk.pcm.is_empty(),
        "packaged {label} decoder must produce PCM on concat init+segment"
    );
    assert_eq!(chunk.spec().sample_rate, 44_100);
    assert_eq!(chunk.spec().channels, 2);
}

#[kithara::test]
fn test_embedded_audio_contains_data() {
    let audio = EmbeddedAudio::get();

    // Verify WAV data exists
    let wav_data = audio.wav();
    assert!(!wav_data.is_empty());

    // Verify MP3 data exists
    let mp3_data = audio.mp3();
    assert!(!mp3_data.is_empty());

    // MP3 should be larger than WAV (our test MP3 is 2.9MB)
    assert!(mp3_data.len() > wav_data.len());
}

fn wav_spec() -> SignalSpec {
    SignalSpec {
        sample_rate: 44_100,
        channels: 2,
        length: SignalSpecLength::Seconds(1.0),
        format: SignalFormat::Wav,
    }
}

fn assert_valid_pcm_samples(samples: &[f32], context: &str) {
    assert!(
        !samples.is_empty(),
        "{context}: decoded PCM chunk must not be empty"
    );
    assert!(
        samples
            .iter()
            .all(|sample| sample.is_finite() && sample.abs() <= 1.25),
        "{context}: decoded PCM contains invalid sample values"
    );
    assert!(
        samples.iter().any(|sample| sample.abs() > 0.01),
        "{context}: decoded PCM unexpectedly looks silent"
    );
}

fn decode_fragment_with_ffmpeg(bytes: &[u8], context: &str) -> Vec<f32> {
    let temp_dir = tempfile::tempdir().expect("create temp dir for ffmpeg decode");
    let input_path = temp_dir.path().join("fragment.m4a");
    fs::write(&input_path, bytes).unwrap_or_else(|error| {
        panic!("{context}: write temporary MP4 fragment failed: {error}");
    });

    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            input_path.to_str().expect("temp path utf-8"),
            "-f",
            "f32le",
            "-acodec",
            "pcm_f32le",
            "-",
        ])
        .output()
        .unwrap_or_else(|error| panic!("{context}: launching ffmpeg failed: {error}"));

    assert!(
        output.status.success(),
        "{context}: ffmpeg decode failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout.len() % 4,
        0,
        "{context}: ffmpeg returned a non-f32le byte stream"
    );

    output
        .stdout
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

fn contains_direction_window(samples: &[f32], channels: usize, expected: SignalDirection) -> bool {
    let frames = samples.len() / channels;
    if frames < 16 {
        return false;
    }

    let window_frames = 2048.min(frames);
    let step_frames = (window_frames / 2).max(1);
    let mut start_frame = 0usize;
    while start_frame + window_frames <= frames {
        let start = start_frame * channels;
        let end = (start_frame + window_frames) * channels;
        if detect_direction(&samples[start..end], channels) == expected {
            return true;
        }
        start_frame += step_frames;
    }
    false
}

fn scan_top_level_box_tags(bytes: &[u8]) -> Vec<String> {
    scan_top_level_box_summaries(bytes)
        .into_iter()
        .map(|summary| summary.tag)
        .collect()
}

#[derive(Debug)]
struct BoxSummary {
    tag: String,
}

fn scan_top_level_box_summaries(bytes: &[u8]) -> Vec<BoxSummary> {
    let mut summaries = Vec::new();
    let mut offset = 0usize;
    while offset + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let tag = String::from_utf8_lossy(&bytes[offset + 4..offset + 8]).to_string();
        summaries.push(BoxSummary { tag });
        if size < 8 {
            break;
        }
        offset = offset.saturating_add(size);
    }
    summaries
}
