use kithara::{
    self,
    encode::{EncoderFactory, PackagedEncodeRequest},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_integration_tests::bufpool_ext::pools;
use kithara_test_fixtures::signal::{Pcm, Wave};

#[kithara::test]
fn encode_packaged_aac_happy_path_emits_monotonic_access_units() {
    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;

    let frame_samples = EncoderFactory::frame_samples(AudioCodec::AacLc)
        .expect("BUG: AacLc must be supported by the packaged encoder");
    let total_frames = 4 * frame_samples;
    let pcm = Pcm::new(SAMPLE_RATE, CHANNELS, total_frames, Wave::Sawtooth);
    let media_info = MediaInfo::builder()
        .codec(AudioCodec::AacLc)
        .container(ContainerFormat::Fmp4)
        .build();

    let pools = pools();
    let encoded = EncoderFactory::encode_packaged(
        &pools,
        &PackagedEncodeRequest::builder()
            .media_info(media_info)
            .pcm(&pcm)
            .timescale(SAMPLE_RATE)
            .bit_rate(128_000)
            .packets_per_segment(2)
            .encoder_delay(0)
            .trailing_delay(0)
            .build(),
    )
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

#[kithara::test]
#[case::he(AudioCodec::AacHe, 64_000)]
#[case::lc(AudioCodec::AacLc, 128_000)]
fn encode_packaged_aac_reuses_injected_pools(#[case] codec: AudioCodec, #[case] bit_rate: u64) {
    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;

    let frame_samples = EncoderFactory::frame_samples(codec).unwrap_or_else(|error| {
        panic!("BUG: {codec:?} must be supported by the packaged encoder: {error}")
    });
    let pcm = Pcm::new(SAMPLE_RATE, CHANNELS, 4 * frame_samples, Wave::Sawtooth);
    let pools = pools();
    let encode = || {
        EncoderFactory::encode_packaged(
            &pools,
            &PackagedEncodeRequest::builder()
                .pcm(&pcm)
                .media_info(
                    MediaInfo::builder()
                        .codec(codec)
                        .container(ContainerFormat::Fmp4)
                        .build(),
                )
                .timescale(SAMPLE_RATE)
                .bit_rate(bit_rate)
                .packets_per_segment(2)
                .encoder_delay(0)
                .trailing_delay(0)
                .build(),
        )
        .unwrap_or_else(|error| panic!("encode_packaged({codec:?}) failed: {error}"))
    };

    let first = encode();
    let after_first = pools.stats().allocated_bytes;
    let second = encode();
    let after_second = pools.stats().allocated_bytes;

    assert!(!first.access_units.is_empty());
    assert!(!second.access_units.is_empty());
    assert_eq!(after_second, after_first);
}
