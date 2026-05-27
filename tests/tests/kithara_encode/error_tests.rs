use kithara_encode::EncodeError;
use kithara_stream::{AudioCodec, ContainerFormat};
use kithara_test_utils::kithara;

#[kithara::test]
fn error_display_mentions_codec() {
    let error = EncodeError::UnsupportedCodec(AudioCodec::AacLc);
    assert_eq!(error.to_string(), "Unsupported codec: AacLc");
}

#[kithara::test]
fn error_display_mentions_container() {
    let error = EncodeError::UnsupportedContainer(ContainerFormat::Fmp4);
    assert_eq!(error.to_string(), "Unsupported container: Fmp4");
}

#[kithara::test]
fn backend_message_wraps_any_string() {
    let error = EncodeError::backend_message("ffmpeg init failed".to_owned());
    assert!(error.to_string().contains("Encoder error"));
}
