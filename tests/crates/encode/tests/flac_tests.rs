use kithara::{
    self,
    encode::{EncoderFactory, PackagedEncodeRequest, normalize_flac_codec_config},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_integration_tests::bufpool_ext::pools;
use kithara_test_fixtures::{
    integration_fixtures::{encoder_saw_flac, flac_config},
    signal::Pcm,
};

const CHANNELS: u16 = 2;
const SAMPLE_RATE: u32 = 48_000;

#[kithara::test]
fn normalize_flac_codec_config_accepts_mp4_metadata_block(flac_config: &'static [u8]) {
    let normalized = normalize_flac_codec_config(flac_config)
        .expect("BUG: hard-coded dfLa payload normalises successfully");
    assert_eq!(normalized.len(), 34);
    assert_eq!(&normalized[..4], &[0x12, 0x00, 0x12, 0x00]);
}

#[kithara::test]
fn encode_packaged_flac_happy_path_emits_monotonic_access_units(encoder_saw_flac: Pcm) {
    let pcm = encoder_saw_flac;
    let media_info = MediaInfo::builder()
        .codec(AudioCodec::Flac)
        .container(ContainerFormat::Fmp4)
        .build();

    let pools = pools();
    let encoded = EncoderFactory::encode_packaged(
        &pools,
        &PackagedEncodeRequest::builder()
            .media_info(media_info)
            .pcm(&pcm)
            .timescale(SAMPLE_RATE)
            .bit_rate(512_000)
            .packets_per_segment(2)
            .encoder_delay(0)
            .trailing_delay(0)
            .build(),
    )
    .unwrap_or_else(|error| panic!("encode_packaged(Flac) failed: {error}"));

    assert_eq!(encoded.media_info.codec, Some(AudioCodec::Flac));
    assert_eq!(encoded.media_info.container, Some(ContainerFormat::Fmp4));
    assert_eq!(encoded.media_info.sample_rate, Some(SAMPLE_RATE));
    assert_eq!(encoded.media_info.channels, Some(CHANNELS));
    assert_eq!(encoded.timescale, SAMPLE_RATE);
    assert_eq!(encoded.packets_per_segment, 2);
    assert_eq!(encoded.codec_config.len(), 34);
    assert!(
        encoded.access_units.len() >= 2,
        "expected multiple FLAC access units, got {}",
        encoded.access_units.len()
    );

    let mut expected_pts = None;
    for unit in &encoded.access_units {
        assert!(!unit.bytes.is_empty(), "access unit payload is empty");
        assert_eq!(unit.pts, unit.dts, "FLAC should not reorder audio packets");
        assert!(unit.is_sync, "FLAC packets should be sync samples");
        assert!(unit.duration > 0, "FLAC packet duration must be positive");

        if let Some(expected_pts) = expected_pts {
            assert_eq!(
                unit.pts, expected_pts,
                "FLAC packet timestamps should be contiguous"
            );
        } else {
            assert_eq!(unit.pts, 0, "FLAC timeline should start at zero");
        }
        expected_pts = Some(unit.pts + u64::from(unit.duration));
    }
}
