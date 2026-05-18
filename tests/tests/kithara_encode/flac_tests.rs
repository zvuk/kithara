use kithara_encode::{
    EncoderFactory, PackagedEncodeRequest,
    codec::{AudioCodec, ContainerFormat, MediaInfo},
    normalize_flac_codec_config,
    test_pcm::SawtoothPcmFixture,
};
use kithara_test_utils::kithara;

const CHANNELS: u16 = 2;
const SAMPLE_RATE: u32 = 48_000;

#[kithara::test]
fn normalize_flac_codec_config_accepts_mp4_metadata_block() {
    let data = [
        0x80, 0x00, 0x00, 0x22, 0x12, 0x00, 0x12, 0x00, 0x00, 0x04, 0x2F, 0x00, 0x09, 0x41, 0x0A,
        0xC4, 0x42, 0xF0, 0x00, 0x00, 0xAC, 0x44, 0x09, 0x1A, 0x92, 0x07, 0x6E, 0xC3, 0xBC, 0x84,
        0x8E, 0x7F, 0x60, 0x75, 0x8D, 0x3A, 0x77, 0x61,
    ];
    let normalized = normalize_flac_codec_config(&data)
        .expect("BUG: hard-coded dfLa payload normalises successfully");
    assert_eq!(normalized.len(), 34);
    assert_eq!(&normalized[..4], &[0x12, 0x00, 0x12, 0x00]);
}

#[kithara::test]
fn encode_packaged_flac_happy_path_emits_monotonic_access_units() {
    let frame_samples = EncoderFactory::frame_samples(AudioCodec::Flac)
        .expect("BUG: Flac must be supported by the packaged encoder");
    let total_frames = 4 * frame_samples;
    let pcm = SawtoothPcmFixture::new(total_frames, SAMPLE_RATE, CHANNELS);
    let media_info = MediaInfo::builder()
        .codec(AudioCodec::Flac)
        .container(ContainerFormat::Fmp4)
        .build();

    let encoded = EncoderFactory::encode_packaged(PackagedEncodeRequest {
        media_info,
        pcm: &pcm,
        timescale: SAMPLE_RATE,
        bit_rate: 512_000,
        packets_per_segment: 2,
        encoder_delay: 0,
        trailing_delay: 0,
    })
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
