use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
use kithara_test_utils::kithara;

use crate::pipeline::decode::format::resolve_target;

fn info(
    codec: Option<AudioCodec>,
    container: Option<ContainerFormat>,
    variant: Option<u32>,
) -> MediaInfo {
    let mut info = MediaInfo::builder()
        .maybe_codec(codec)
        .maybe_container(container)
        .build();
    info.variant_index = variant;
    info
}

#[kithara::test]
fn no_change_when_variant_index_matches() {
    let cached = info(
        Some(AudioCodec::AacLc),
        Some(ContainerFormat::Fmp4),
        Some(0),
    );
    let current = info(
        Some(AudioCodec::AacLc),
        Some(ContainerFormat::Fmp4),
        Some(0),
    );
    assert!(resolve_target(Some(&cached), &current).is_none());
}

#[kithara::test]
fn same_codec_fmp4_variant_change_recreates_boundary() {
    let cached = info(
        Some(AudioCodec::AacLc),
        Some(ContainerFormat::Fmp4),
        Some(0),
    );
    let current = info(
        Some(AudioCodec::AacLc),
        Some(ContainerFormat::Fmp4),
        Some(1),
    );
    let target = resolve_target(Some(&cached), &current)
        .expect("same-codec fMP4 variant change must re-prime the demuxer");
    assert_eq!(target.variant_index, Some(1));
    assert_eq!(target.codec, Some(AudioCodec::AacLc));
    assert_eq!(target.container, Some(ContainerFormat::Fmp4));
}

#[kithara::test]
fn same_codec_wav_variant_change_is_byte_continuity() {
    let cached = info(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav), Some(0));
    let current = info(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav), Some(1));
    assert!(resolve_target(Some(&cached), &current).is_none());
}

#[kithara::test]
fn variant_change_keeps_cached_codec_and_container_when_current_disagrees() {
    let cached = info(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav), Some(0));
    let current = info(None, Some(ContainerFormat::Fmp4), Some(1));
    let target = resolve_target(Some(&cached), &current).expect("variant change must trigger");
    assert_eq!(target.codec, Some(AudioCodec::Pcm));
    assert_eq!(target.container, Some(ContainerFormat::Wav));
    assert_eq!(target.variant_index, Some(1));
}

#[kithara::test]
fn variant_change_falls_back_to_current_when_cached_lacks_codec_or_container() {
    let cached = info(None, None, Some(0));
    let current = info(
        Some(AudioCodec::AacLc),
        Some(ContainerFormat::Fmp4),
        Some(2),
    );
    let target = resolve_target(Some(&cached), &current).expect("variant change must trigger");
    assert_eq!(target.codec, Some(AudioCodec::AacLc));
    assert_eq!(target.container, Some(ContainerFormat::Fmp4));
    assert_eq!(target.variant_index, Some(2));
}

#[kithara::test]
fn no_cached_uses_current_directly() {
    let current = info(
        Some(AudioCodec::AacLc),
        Some(ContainerFormat::Fmp4),
        Some(1),
    );
    let target = resolve_target(None, &current).expect("None cached + Some(variant) must trigger");
    assert_eq!(target, current);
}

#[kithara::test]
fn explicit_codec_change_takes_current_codec() {
    let cached = info(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4), None);
    let current = info(Some(AudioCodec::Flac), Some(ContainerFormat::Fmp4), None);
    let target = resolve_target(Some(&cached), &current).expect("codec change must trigger");
    assert_eq!(target.codec, Some(AudioCodec::Flac));
    assert_eq!(target.container, Some(ContainerFormat::Fmp4));
}

#[kithara::test]
fn current_codec_none_is_not_a_codec_change() {
    let cached = info(
        Some(AudioCodec::AacLc),
        Some(ContainerFormat::Fmp4),
        Some(0),
    );
    let current = info(None, Some(ContainerFormat::Fmp4), Some(0));
    assert!(resolve_target(Some(&cached), &current).is_none());
}

#[kithara::test]
fn no_change_when_neither_side_has_variant() {
    let cached = info(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4), None);
    let current = info(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4), None);
    assert!(resolve_target(Some(&cached), &current).is_none());
}
