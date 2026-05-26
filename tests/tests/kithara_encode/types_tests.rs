use kithara_encode::BytesEncodeTarget;
use kithara_integration_tests::encode_ext::BytesEncodeTargetExt;
use kithara_stream::{AudioCodec, ContainerFormat};
use kithara_test_utils::kithara;

#[kithara::test]
fn bytes_target_maps_to_expected_media_info() {
    let info = BytesEncodeTarget::M4a.media_info(44_100, 2);
    assert_eq!(info.codec, Some(AudioCodec::AacLc));
    assert_eq!(info.container, Some(ContainerFormat::Mp4));
    assert_eq!(info.sample_rate, Some(44_100));
    assert_eq!(info.channels, Some(2));
}

#[kithara::test]
#[case::mp3(BytesEncodeTarget::Mp3, "mp3", "audio/mpeg", Some(128_000))]
#[case::flac(BytesEncodeTarget::Flac, "flac", "audio/flac", None)]
#[case::aac(BytesEncodeTarget::Aac, "aac", "audio/aac", Some(128_000))]
#[case::m4a(BytesEncodeTarget::M4a, "m4a", "audio/mp4", Some(128_000))]
fn bytes_target_defaults_match_route_contract(
    #[case] target: BytesEncodeTarget,
    #[case] ext: &str,
    #[case] mime: &str,
    #[case] bit_rate: Option<u64>,
) {
    assert_eq!(target.extension(), ext);
    assert_eq!(target.content_type(), mime);
    assert_eq!(target.default_bit_rate(), bit_rate);
}
