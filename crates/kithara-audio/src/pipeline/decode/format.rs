use kithara_decode::DecodeError;
use kithara_stream::{MediaInfo, SeekObserve, StreamType};

use crate::pipeline::{
    seek::anchor::variant_boundary,
    stream::shared::SharedStream,
    track_fsm::{DecoderSession, RecreateCause, RecreateNext, RecreateState},
};

pub(crate) enum FormatDecision {
    None,
    Recreate(RecreateState),
}

pub(crate) fn detect<T: StreamType>(
    stream: &SharedStream<T>,
    session: &DecoderSession,
    seek: &dyn SeekObserve,
) -> FormatDecision {
    if seek.is_pending() && session.installed_at_seek_epoch == seek.epoch() {
        return FormatDecision::None;
    }
    let Some(current) = stream.media_info() else {
        return FormatDecision::None;
    };
    let Some(media_info) = resolve_target(session.media_info.as_ref(), &current) else {
        return FormatDecision::None;
    };
    let Ok(range) = stream.format_change_segment_range() else {
        return FormatDecision::None;
    };
    FormatDecision::Recreate(RecreateState {
        cause: RecreateCause::FormatBoundary,
        media_info,
        next: RecreateNext::Decode,
        offset: range.start,
    })
}

pub(crate) fn handle_variant_change<T: StreamType>(
    stream: &SharedStream<T>,
    session: &DecoderSession,
    seek: &dyn SeekObserve,
    no_change: &DecodeError,
) -> Result<RecreateState, DecodeError> {
    if let FormatDecision::Recreate(recreate) = detect(stream, session, seek) {
        return Ok(recreate);
    }
    if !seek.is_pending()
        && let Some(target) = stream.variant_change_target()
        && let Some(session_info) = session.media_info.clone()
        && let Some(session_variant) = session_info.variant_index
        && usize::try_from(session_variant) == Ok(target)
        && let Some(current) = stream.media_info()
        && current.variant_index == Some(session_variant)
        && let Ok(range) = stream.format_change_segment_range()
    {
        let mut media_info = session_info;
        if media_info.codec.is_none() {
            media_info.codec = current.codec;
        }
        if media_info.container.is_none() {
            media_info.container = current.container;
        }
        return Ok(RecreateState {
            cause: RecreateCause::FormatBoundary,
            media_info,
            next: RecreateNext::Decode,
            offset: range.start,
        });
    }
    let _ = no_change;
    Err(DecodeError::Interrupted)
}

pub(crate) fn resolve_target(cached: Option<&MediaInfo>, current: &MediaInfo) -> Option<MediaInfo> {
    let variant_changed = cached.map_or_else(
        || current.variant_index.is_some(),
        |value| value.variant_index != current.variant_index,
    );
    let codec_changed = matches!(
        (cached.and_then(|value| value.codec), current.codec),
        (Some(from), Some(to)) if from != to
    );
    let same_codec = matches!(
        (cached.and_then(|value| value.codec), current.codec),
        (Some(from), Some(to)) if from == to
    );
    let container = current
        .container
        .or_else(|| cached.and_then(|value| value.container));
    if !variant_boundary(variant_changed, same_codec, container) && !codec_changed {
        return None;
    }
    Some(cached.map_or_else(
        || current.clone(),
        |value| {
            let mut target = value.clone();
            target.variant_index = current.variant_index;
            if codec_changed || target.codec.is_none() {
                target.codec = current.codec;
            }
            if target.container.is_none() {
                target.container = current.container;
            }
            target
        },
    ))
}

#[cfg(test)]
mod tests {
    use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
    use kithara_test_utils::kithara;

    use super::resolve_target;

    fn info(
        codec: Option<AudioCodec>,
        container: Option<ContainerFormat>,
        variant: Option<u32>,
    ) -> MediaInfo {
        let mut info = MediaInfo::new(codec, container);
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
        let target =
            resolve_target(None, &current).expect("missing cache with variant must trigger");
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
}
