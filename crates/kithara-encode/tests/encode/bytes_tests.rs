use kithara_encode::{BytesEncodeRequest, BytesEncodeTarget, EncoderFactory};
use kithara_test_fixtures::{
    integration_fixtures::{encoder_saw_aac, encoder_second},
    signal::Pcm,
};
use kithara_test_utils::kithara;

#[kithara::test]
fn encode_bytes_happy_paths_return_expected_metadata_and_container_markers(encoder_saw_aac: Pcm) {
    const SAMPLE_RATE: u32 = 48_000;
    const CHANNELS: u16 = 2;

    let pcm = encoder_saw_aac;
    let cases = [
        (BytesEncodeTarget::Mp3, "audio/mpeg"),
        (BytesEncodeTarget::Flac, "audio/flac"),
        (BytesEncodeTarget::Aac, "audio/aac"),
        (BytesEncodeTarget::M4a, "audio/mp4"),
        (BytesEncodeTarget::Alac, "audio/mp4"),
        (BytesEncodeTarget::Flac24, "audio/flac"),
        (BytesEncodeTarget::Vorbis, "audio/ogg"),
        (BytesEncodeTarget::Opus, "audio/ogg"),
        (BytesEncodeTarget::Wav16, "audio/wav"),
        (BytesEncodeTarget::Wav24, "audio/wav"),
        (BytesEncodeTarget::Wav32, "audio/wav"),
        (BytesEncodeTarget::WavFloat32, "audio/wav"),
        (BytesEncodeTarget::Aiff16, "audio/aiff"),
        #[cfg(feature = "monkeys-audio")]
        (BytesEncodeTarget::Ape, "audio/ape"),
    ];

    for (target, content_type) in cases {
        let encoded = EncoderFactory::encode_bytes(&BytesEncodeRequest {
            target,
            pcm: &pcm,
            bit_rate: None,
        })
        .unwrap_or_else(|error| panic!("encode_bytes({target:?}) failed: {error}"));

        assert!(!encoded.bytes.is_empty(), "{target:?} payload is empty");
        assert_eq!(encoded.content_type, content_type);
        assert_eq!(encoded.media_info.codec, Some(target.codec()));
        assert_eq!(encoded.media_info.container, Some(target.container()));
        assert_eq!(encoded.media_info.sample_rate, Some(SAMPLE_RATE));
        assert_eq!(encoded.media_info.channels, Some(CHANNELS));

        assert_container_marker(target, &encoded.bytes);
    }
}

#[kithara::test]
fn encode_bytes_honors_explicit_bit_rate_across_lossy_range(encoder_second: Pcm) {
    let pcm = encoder_second;

    let bit_rates = [96_000u64, 128_000, 192_000, 256_000, 320_000];
    let lossy_targets = [
        BytesEncodeTarget::Mp3,
        BytesEncodeTarget::Aac,
        BytesEncodeTarget::M4a,
    ];

    for target in lossy_targets {
        for bit_rate in bit_rates {
            let encoded = EncoderFactory::encode_bytes(&BytesEncodeRequest {
                target,
                pcm: &pcm,
                bit_rate: Some(bit_rate),
            })
            .unwrap_or_else(|error| panic!("encode_bytes({target:?}, {bit_rate}) failed: {error}"));

            assert!(
                !encoded.bytes.is_empty(),
                "{target:?} @ {bit_rate}: empty output"
            );
            assert_container_marker(target, &encoded.bytes);

            let expected_min = (usize::try_from(bit_rate).unwrap_or(0) / 8) / 8;
            let expected_max = (usize::try_from(bit_rate).unwrap_or(0) / 8) * 4;
            assert!(
                encoded.bytes.len() >= expected_min,
                "{target:?} @ {bit_rate}: output {} bytes < expected_min {expected_min} bps \
                 (1s PCM @ {bit_rate} bps should yield ≥ {expected_min} bytes)",
                encoded.bytes.len()
            );
            assert!(
                encoded.bytes.len() <= expected_max,
                "{target:?} @ {bit_rate}: output {} bytes > expected_max {expected_max} bps",
                encoded.bytes.len()
            );
        }
    }
}

fn assert_container_marker(target: BytesEncodeTarget, bytes: &[u8]) {
    match target {
        BytesEncodeTarget::Mp3 => assert!(
            bytes.starts_with(b"ID3")
                || (bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0),
            "MP3 output is missing an ID3 tag or MPEG frame sync"
        ),
        BytesEncodeTarget::Flac | BytesEncodeTarget::Flac24 => {
            assert!(
                bytes.starts_with(b"fLaC"),
                "FLAC output is missing the `fLaC` marker"
            );
        }
        BytesEncodeTarget::Aac => assert!(
            bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xF0) == 0xF0,
            "AAC output is missing an ADTS sync word"
        ),
        BytesEncodeTarget::M4a => assert_mp4(bytes, b"mp4a", "M4A"),
        BytesEncodeTarget::Alac => assert_mp4(bytes, b"alac", "ALAC"),
        BytesEncodeTarget::Vorbis => {
            assert!(bytes.starts_with(b"OggS"));
            assert!(bytes.windows(7).any(|window| window == b"\x01vorbis"));
        }
        BytesEncodeTarget::Opus => {
            assert!(bytes.starts_with(b"OggS"));
            assert!(bytes.windows(8).any(|window| window == b"OpusHead"));
        }
        BytesEncodeTarget::Wav16
        | BytesEncodeTarget::Wav24
        | BytesEncodeTarget::Wav32
        | BytesEncodeTarget::WavFloat32 => {
            assert!(bytes.starts_with(b"RIFF"));
            assert_eq!(&bytes[8..12], b"WAVE");
        }
        BytesEncodeTarget::Aiff16 => {
            assert!(bytes.starts_with(b"FORM"));
            assert_eq!(&bytes[8..12], b"AIFF");
        }
        BytesEncodeTarget::Ape => assert!(bytes.starts_with(b"MAC ")),
    }
}

/// MP4 container boxes plus the sample-entry fourcc that names the codec. The
/// boxes alone cannot tell an AAC body from a lossless one — both are `.m4a`.
fn assert_mp4(bytes: &[u8], sample_entry: &[u8; 4], label: &str) {
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|window| window == needle);
    assert!(
        has(b"ftyp") && has(b"mdat"),
        "{label} output is missing MP4 container boxes"
    );
    assert!(
        has(sample_entry),
        "{label} output is missing its `{}` sample entry",
        String::from_utf8_lossy(sample_entry)
    );
}
