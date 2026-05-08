use kithara_encode::{
    BytesEncodeRequest, BytesEncodeTarget, EncoderFactory, test_pcm::SawtoothPcmFixture,
};
use kithara_test_utils::kithara;

#[kithara::test]
fn encode_bytes_happy_paths_return_expected_metadata_and_container_markers() {
    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;
    const AAC_FRAME_SAMPLES: usize = 1024;

    let pcm = SawtoothPcmFixture::new(4 * AAC_FRAME_SAMPLES, SAMPLE_RATE, CHANNELS);
    let cases = [
        BytesEncodeTarget::Mp3,
        BytesEncodeTarget::Flac,
        BytesEncodeTarget::Aac,
        BytesEncodeTarget::M4a,
    ];

    for target in cases {
        let encoded = EncoderFactory::encode_bytes(BytesEncodeRequest {
            target,
            pcm: &pcm,
            bit_rate: None,
        })
        .unwrap_or_else(|error| panic!("encode_bytes({target:?}) failed: {error}"));

        assert!(!encoded.bytes.is_empty(), "{target:?} payload is empty");
        assert_eq!(encoded.content_type, target.content_type());
        assert_eq!(encoded.media_info.codec, Some(target.codec()));
        assert_eq!(encoded.media_info.container, Some(target.container()));
        assert_eq!(encoded.media_info.sample_rate, Some(SAMPLE_RATE));
        assert_eq!(encoded.media_info.channels, Some(CHANNELS));

        assert_container_marker(target, &encoded.bytes);
    }
}

fn assert_container_marker(target: BytesEncodeTarget, bytes: &[u8]) {
    match target {
        BytesEncodeTarget::Mp3 => assert!(
            bytes.starts_with(b"ID3")
                || (bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0),
            "MP3 output is missing an ID3 tag or MPEG frame sync"
        ),
        BytesEncodeTarget::Flac => {
            assert!(
                bytes.starts_with(b"fLaC"),
                "FLAC output is missing the `fLaC` marker"
            );
        }
        BytesEncodeTarget::Aac => assert!(
            bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xF0) == 0xF0,
            "AAC output is missing an ADTS sync word"
        ),
        BytesEncodeTarget::M4a => assert!(
            bytes.windows(4).any(|window| window == b"ftyp")
                && bytes.windows(4).any(|window| window == b"mdat"),
            "M4A output is missing MP4 container boxes"
        ),
    }
}
