use super::*;

#[kithara::fixture]
pub async fn trimmed() -> (TestServerHelper, CreatedHls) {
    gapless_source(
        Some(AAC_GAPLESS_ENCODER_DELAY),
        Some(AAC_GAPLESS_TRAILING_DELAY),
        0,
        GaplessEncoding::default(),
    )
    .await
}

#[kithara::fixture]
pub async fn trimmed_stitch() -> (TestServerHelper, CreatedHls) {
    gapless_source(
        Some(AAC_GAPLESS_ENCODER_DELAY),
        Some(AAC_GAPLESS_TRAILING_DELAY),
        u64::try_from(crate::gapless::generated_aac_elst_visible_frames())
            .expect("stitch frame fits u64"),
        GaplessEncoding::default(),
    )
    .await
}

#[kithara::fixture]
pub async fn trimmed_next() -> (TestServerHelper, CreatedHls) {
    gapless_source(
        Some(AAC_GAPLESS_ENCODER_DELAY),
        Some(AAC_GAPLESS_TRAILING_DELAY),
        u64::try_from(expected_visible_frames(
            AAC_GAPLESS_ENCODER_DELAY,
            AAC_GAPLESS_TRAILING_DELAY,
        ))
        .expect("visible frames fit u64"),
        GaplessEncoding::default(),
    )
    .await
}

#[kithara::fixture]
pub async fn primed() -> (TestServerHelper, CreatedHls) {
    gapless_source(
        Some(AAC_GAPLESS_ENCODER_DELAY),
        None,
        0,
        GaplessEncoding::default(),
    )
    .await
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
pub async fn apple_source(start_frame: u64) -> (TestServerHelper, CreatedHls) {
    let encoder_delay = Some(AAC_GAPLESS_ENCODER_DELAY);
    let trailing_delay: Option<u32> = None;
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(APPLE_FUSED_DEFICIT_SEGMENTS)
                .segment_duration_secs(APPLE_FUSED_DEFICIT_SEGMENT_SECS)
                .packaged_audio(PackagedAudioRequest {
                    codec: AudioCodec::AacLc,
                    sample_rate: FUSED_FIXTURE_SOURCE_RATE,
                    channels: GAPLESS_CHANNELS,
                    start_frame: NonZeroU32::new(
                        u32::try_from(start_frame).expect("start_frame fits u32"),
                    ),
                    timescale: Some(FUSED_FIXTURE_SOURCE_RATE),
                    bit_rate: Some(128_000),
                    encoder_delay: encoder_delay.and_then(NonZeroU32::new),
                    trailing_delay: trailing_delay.and_then(NonZeroU32::new),
                    source: PackagedAudioSource::Signal(PackagedSignal::Sine { freq_hz: SINE_HZ }),
                    gapless_encoding: GaplessEncoding::default(),
                    variant_overrides: Vec::new(),
                }),
        )
        .await
        .expect("create Apple fused gapless HLS fixture");

    (server, created)
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
#[kithara::fixture]
pub async fn apple_probe() -> (TestServerHelper, CreatedHls) {
    apple_source(0).await
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
#[kithara::fixture]
pub async fn apple_first() -> (TestServerHelper, CreatedHls) {
    apple_source(0).await
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
#[kithara::fixture]
pub async fn apple_next() -> (TestServerHelper, CreatedHls) {
    apple_source(APPLE_FUSED_DEFICIT_SOURCE_FRAMES).await
}
