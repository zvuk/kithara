use kithara_encode::{
    EncoderFactory, PackagedEncodeRequest,
    codec::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_integration_tests::encode_test_pcm::SawtoothPcmFixture;
use kithara_test_utils::kithara;

#[kithara::test]
fn encode_packaged_aac_happy_path_emits_monotonic_access_units() {
    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;

    let frame_samples = EncoderFactory::frame_samples(AudioCodec::AacLc)
        .expect("BUG: AacLc must be supported by the packaged encoder");
    let total_frames = 4 * frame_samples;
    let pcm = SawtoothPcmFixture::new(total_frames, SAMPLE_RATE, CHANNELS);
    let media_info = MediaInfo::builder()
        .codec(AudioCodec::AacLc)
        .container(ContainerFormat::Fmp4)
        .build();

    let encoded = EncoderFactory::encode_packaged(PackagedEncodeRequest {
        media_info,
        pcm: &pcm,
        timescale: SAMPLE_RATE,
        bit_rate: 128_000,
        packets_per_segment: 2,
        encoder_delay: 0,
        trailing_delay: 0,
    })
    .unwrap_or_else(|error| panic!("encode_packaged(AacLc) failed: {error}"));

    assert_eq!(encoded.media_info.codec, Some(AudioCodec::AacLc));
    assert_eq!(encoded.media_info.container, Some(ContainerFormat::Fmp4));
    assert_eq!(encoded.media_info.sample_rate, Some(SAMPLE_RATE));
    assert_eq!(encoded.media_info.channels, Some(CHANNELS));
    assert_eq!(encoded.timescale, SAMPLE_RATE);
    assert_eq!(encoded.bit_rate, 128_000);
    assert_eq!(encoded.packets_per_segment, 2);
    assert!(encoded.codec_config.is_empty());
    assert!(
        encoded.access_units.len() >= 2,
        "expected multiple AAC access units, got {}",
        encoded.access_units.len()
    );

    let mut expected_pts = None;
    for unit in &encoded.access_units {
        assert!(!unit.bytes.is_empty(), "access unit payload is empty");
        assert_eq!(unit.pts, unit.dts, "AAC should not reorder audio packets");
        assert_eq!(
            unit.duration,
            u32::try_from(frame_samples).expect("AAC frame size fits u32"),
            "AAC-LC packets should use the natural frame duration"
        );

        if let Some(expected_pts) = expected_pts {
            assert_eq!(
                unit.pts, expected_pts,
                "AAC packet timestamps should be contiguous"
            );
        } else {
            assert_eq!(unit.pts, 0, "AAC timeline should start at zero");
        }
        expected_pts = Some(unit.pts + u64::from(unit.duration));
    }
}
