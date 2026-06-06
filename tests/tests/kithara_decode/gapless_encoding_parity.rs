use std::{io::Cursor, num::NonZeroU32};

use kithara::{decode::probe_mp4_gapless, platform::time::Duration};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper,
    fixture_protocol::{
        GaplessEncoding, PackagedAudioRequest, PackagedAudioSource, PackagedSignal,
    },
};
use kithara_stream::AudioCodec;
use reqwest::Client;

use crate::gapless_common::{
    AAC_FRAME_SAMPLES, AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_SEGMENT_SECS, AAC_GAPLESS_SEGMENTS,
    AAC_GAPLESS_TRAILING_DELAY, GAPLESS_CHANNELS, GAPLESS_SAMPLE_RATE,
};

#[kithara::test(
    flash(false),
    native,
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn gapless_encoding_variants_yield_matching_decoder_metadata() {
    let server = TestServerHelper::new().await;

    let edts = probe_init_segment(&server, GaplessEncoding::Edts).await;
    let itunsmpb = probe_init_segment(&server, GaplessEncoding::ItunSmpb).await;
    let both = probe_init_segment(&server, GaplessEncoding::Both).await;
    let none = probe_init_segment(&server, GaplessEncoding::None).await;

    let edts_info = edts.expect("Edts fixture must yield gapless info");
    let itunsmpb_info = itunsmpb.expect("ItunSmpb fixture must yield gapless info");
    let both_info = both.expect("Both fixture must yield gapless info");

    assert_eq!(
        edts_info, itunsmpb_info,
        "edts and iTunSMPB must report identical GaplessInfo for the same fixture"
    );
    assert_eq!(
        edts_info, both_info,
        "Both must report identical GaplessInfo to a single-source fixture (elst wins)"
    );
    let expected_leading = u64::from(AAC_GAPLESS_ENCODER_DELAY)
        + u64::try_from(AAC_FRAME_SAMPLES).expect("AAC frame samples fit u64");
    assert_eq!(
        edts_info.leading_frames, expected_leading,
        "leading_frames must equal encoder_delay + native AAC priming"
    );
    assert_eq!(
        u32::try_from(edts_info.trailing_frames).expect("trailing fits u32"),
        AAC_GAPLESS_TRAILING_DELAY,
        "trailing_frames must equal the configured trailing_delay"
    );

    assert!(
        none.is_none(),
        "None encoding must produce a fixture without gapless metadata, got {none:?}"
    );
}

async fn probe_init_segment(
    server: &TestServerHelper,
    encoding: GaplessEncoding,
) -> Option<kithara::decode::GaplessInfo> {
    let init_bytes = build_init_segment(server, encoding).await;
    probe_mp4_gapless(&mut Cursor::new(init_bytes)).expect("probe init segment")
}

async fn build_init_segment(server: &TestServerHelper, encoding: GaplessEncoding) -> Vec<u8> {
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(AAC_GAPLESS_SEGMENTS)
                .segment_duration_secs(AAC_GAPLESS_SEGMENT_SECS)
                .packaged_audio(PackagedAudioRequest {
                    codec: AudioCodec::AacLc,
                    sample_rate: GAPLESS_SAMPLE_RATE,
                    channels: GAPLESS_CHANNELS,
                    start_frame: None,
                    timescale: Some(GAPLESS_SAMPLE_RATE),
                    bit_rate: Some(128_000),
                    encoder_delay: NonZeroU32::new(AAC_GAPLESS_ENCODER_DELAY),
                    trailing_delay: NonZeroU32::new(AAC_GAPLESS_TRAILING_DELAY),
                    source: PackagedAudioSource::Signal(PackagedSignal::Sine { freq_hz: 1_000.0 }),
                    gapless_encoding: encoding,
                    variant_overrides: Vec::new(),
                }),
        )
        .await
        .expect("create gapless encoding parity fixture");

    let client = Client::new();
    let url = created.init_url(0).to_string();
    let response = client.get(&url).send().await.expect("fetch init segment");
    response
        .bytes()
        .await
        .expect("read init segment bytes")
        .to_vec()
}
