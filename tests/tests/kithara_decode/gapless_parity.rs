use kithara::{decode::DecoderConfig, platform::time::Duration};
use kithara_test_utils::{
    SignalFormat, SignalSpec, SignalSpecLength, TestServerHelper, fixture_protocol::PackagedSignal,
};
use reqwest::Client;

use crate::gapless_common::{
    AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_TRAILING_DELAY, GAPLESS_CHANNELS, GAPLESS_SAMPLE_RATE,
    assert_frames_within, create_decoder_from_media_info, create_decoder_with_probe,
    decode_visible_frames, generated_aac_elst_fixture,
};

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn generated_aac_elst_visible_frames_match_generated_timing_across_factory_paths() {
    let server = TestServerHelper::new().await;
    let fixture =
        generated_aac_elst_fixture(&server, PackagedSignal::Sine { freq_hz: 1_000.0 }, 0).await;

    let direct = create_decoder_from_media_info(&fixture, DecoderConfig::default())
        .expect("create direct AAC fMP4 decoder");
    let direct_gapless = direct
        .track_info()
        .gapless
        .expect("direct gapless metadata");
    assert_eq!(
        direct_gapless.leading_frames,
        u64::from(AAC_GAPLESS_ENCODER_DELAY)
    );
    assert_eq!(
        direct_gapless.trailing_frames,
        u64::from(AAC_GAPLESS_TRAILING_DELAY)
    );
    let direct_pcm = decode_visible_frames(direct).expect("decode direct AAC fMP4");

    let probe = create_decoder_with_probe(
        fixture.bytes.clone(),
        "m4a",
        DecoderConfig {
            hint: Some("m4a".to_string()),
            ..DecoderConfig::default()
        },
    )
    .expect("create probe AAC fMP4 decoder");
    let probe_gapless = probe.track_info().gapless.expect("probe gapless metadata");
    assert_eq!(probe_gapless, direct_gapless);
    let probe_pcm = decode_visible_frames(probe).expect("decode probe AAC fMP4");

    let preferred = create_decoder_from_media_info(
        &fixture,
        DecoderConfig {
            prefer_hardware: true,
            ..DecoderConfig::default()
        },
    )
    .expect("create preferred-backend AAC fMP4 decoder");
    let preferred_pcm = decode_visible_frames(preferred).expect("decode preferred AAC fMP4");

    assert_frames_within(
        direct_pcm.frames,
        fixture.expected_visible_frames,
        2,
        "direct AAC fMP4",
    );
    assert_frames_within(
        probe_pcm.frames,
        fixture.expected_visible_frames,
        2,
        "probe AAC fMP4",
    );
    assert_frames_within(
        preferred_pcm.frames,
        fixture.expected_visible_frames,
        2,
        "preferred AAC fMP4",
    );
    assert_frames_within(
        probe_pcm.frames,
        direct_pcm.frames,
        2,
        "probe vs direct AAC",
    );
    assert_frames_within(
        preferred_pcm.frames,
        direct_pcm.frames,
        2,
        "preferred vs direct AAC",
    );
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::mp3(SignalFormat::Mp3, "mp3", 48_000)]
#[case::flac(SignalFormat::Flac, "flac", 48_000)]
async fn generated_encoded_signal_visible_frames_match_requested_signal_frames(
    #[case] format: SignalFormat,
    #[case] hint: &'static str,
    #[case] expected_frames: usize,
) {
    let server = TestServerHelper::new().await;
    let spec = SignalSpec {
        sample_rate: GAPLESS_SAMPLE_RATE,
        channels: GAPLESS_CHANNELS,
        length: SignalSpecLength::Frames(expected_frames),
        format,
    };

    let bytes = Client::new()
        .get(server.sine(&spec, 1_000.0).await)
        .send()
        .await
        .expect("fetch encoded signal")
        .error_for_status()
        .expect("encoded signal response status")
        .bytes()
        .await
        .expect("read encoded signal bytes")
        .to_vec();

    let default_pcm = decode_visible_frames(
        create_decoder_with_probe(bytes.clone(), hint, DecoderConfig::default())
            .expect("create default decoder"),
    )
    .expect("decode default path");
    let preferred_pcm = decode_visible_frames(
        create_decoder_with_probe(
            bytes,
            hint,
            DecoderConfig {
                prefer_hardware: true,
                ..DecoderConfig::default()
            },
        )
        .expect("create preferred decoder"),
    )
    .expect("decode preferred path");

    assert_frames_within(default_pcm.frames, expected_frames, 2, hint);
    assert_frames_within(preferred_pcm.frames, expected_frames, 2, hint);
    assert_frames_within(
        preferred_pcm.frames,
        default_pcm.frames,
        2,
        "preferred parity",
    );
}
