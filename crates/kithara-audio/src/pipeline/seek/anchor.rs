use kithara_decode::DecodeError;
use kithara_stream::{ContainerFormat, MediaInfo, SourceSeekAnchor, StreamType};

use crate::pipeline::{
    decode::DecoderSession,
    rebuild::{RecreateCause, RecreateNext, RecreateState},
    seek::SeekRequest,
    stream::shared::SharedStream,
};

pub(crate) enum AnchorPlan {
    Failed {
        context: &'static str,
        error: DecodeError,
    },
    Recreate(RecreateState),
    Seek,
}

pub(crate) fn format_boundary(
    current: Option<&MediaInfo>,
    target: Option<&MediaInfo>,
    target_variant: Option<usize>,
) -> bool {
    let current_codec = current.and_then(|info| info.codec);
    let target_codec = target.and_then(|info| info.codec);
    let current_variant = current
        .and_then(|info| info.variant_index)
        .and_then(|variant| usize::try_from(variant).ok());
    let codec_changed = matches!(
        (current_codec, target_codec),
        (Some(from), Some(to)) if from != to
    );
    let same_codec = matches!(
        (current_codec, target_codec),
        (Some(from), Some(to)) if from == to
    );
    let variant_changed = matches!(
        (current_variant, target_variant),
        (Some(from), Some(to)) if from != to
    );
    let container = target
        .and_then(|info| info.container)
        .or_else(|| current.and_then(|info| info.container));
    codec_changed || variant_boundary(variant_changed, same_codec, container)
}

pub(crate) fn variant_boundary(
    changed: bool,
    same_codec: bool,
    container: Option<ContainerFormat>,
) -> bool {
    changed
        && (!same_codec
            || container.is_some_and(|format| format != ContainerFormat::Wav && needs_init(format)))
}

pub(crate) fn needs_init(container: ContainerFormat) -> bool {
    matches!(
        container,
        ContainerFormat::Fmp4
            | ContainerFormat::Mp4
            | ContainerFormat::Wav
            | ContainerFormat::Mkv
            | ContainerFormat::Caf
    )
}

pub(crate) fn stale(anchor_variant: Option<usize>, current_variant: Option<usize>) -> bool {
    matches!(
        (anchor_variant, current_variant),
        (Some(anchor), Some(current)) if anchor != current
    )
}

pub(crate) fn recreate_offset<T: StreamType>(
    stream: &SharedStream<T>,
    container: Option<ContainerFormat>,
    codec_changed: bool,
    anchor: u64,
) -> Option<u64> {
    let requires_init = container.is_some_and(needs_init);
    let init = stream
        .format_change_segment_range()
        .ok()
        .map(|range| range.start);
    if requires_init {
        init
    } else if codec_changed {
        Some(init.unwrap_or(anchor))
    } else {
        Some(anchor)
    }
}

pub(crate) fn resolve<T: StreamType>(
    stream: &SharedStream<T>,
    session: &DecoderSession,
    request: SeekRequest,
    anchor: SourceSeekAnchor,
) -> AnchorPlan {
    let stream_info = stream.media_info();
    let target_variant = anchor.variant_index.or_else(|| {
        stream_info
            .as_ref()
            .and_then(|info| info.variant_index)
            .and_then(|variant| usize::try_from(variant).ok())
    });
    if !format_boundary(
        session.media_info.as_ref(),
        stream_info.as_ref(),
        target_variant,
    ) {
        return AnchorPlan::Seek;
    }
    let current_codec = session.media_info.as_ref().and_then(|info| info.codec);
    let target_codec = stream_info.as_ref().and_then(|info| info.codec);
    let codec_changed = matches!((current_codec, target_codec), (Some(a), Some(b)) if a != b);
    let container = stream_info
        .as_ref()
        .and_then(|info| info.container)
        .or_else(|| session.media_info.as_ref().and_then(|info| info.container));
    let Some(offset) = recreate_offset(stream, container, codec_changed, anchor.byte_offset) else {
        return AnchorPlan::Failed {
            context: "seek anchor alignment: no init segment range",
            error: DecodeError::InvalidData {
                detail: "seek anchor alignment: decoder recreate has no init segment range",
            },
        };
    };
    let variant = target_variant.and_then(|value| u32::try_from(value).ok());
    let Some(mut media_info) = stream_info.or_else(|| session.media_info.clone()) else {
        return AnchorPlan::Failed {
            context: "seek anchor alignment failed",
            error: DecodeError::InvalidData {
                detail: "seek anchor alignment: variant/codec changed but media info unavailable",
            },
        };
    };
    if let Some(variant) = variant {
        media_info.variant_index = Some(variant);
    }
    AnchorPlan::Recreate(RecreateState {
        cause: RecreateCause::VariantSwitch,
        media_info,
        next: RecreateNext::Seek(request),
        offset,
    })
}
