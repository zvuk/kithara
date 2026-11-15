mod convert;
pub(crate) use convert::peak_bitrate_cap;
mod records;

pub use records::{
    FfiAbrMode, FfiActionAtItemEnd, FfiAdvanceReason, FfiAudioCodecKind, FfiCancelReason,
    FfiContainerKind, FfiCrossfadeCurve, FfiCrossfadeSettings, FfiDecodeErrorClass,
    FfiDecodeErrorKind, FfiDecoderBackend, FfiDecoderChangeCause, FfiDuckingMode, FfiError,
    FfiEvictReason, FfiFrameDomain, FfiInterruptionKind, FfiItemConfig, FfiItemEvent,
    FfiItemLoadResult, FfiItemState, FfiItemStatus, FfiKeyFailureStage, FfiKeyOptions, FfiKeyRule,
    FfiKeySource, FfiPlaybackOrder, FfiPlaybackResamplerKind, FfiPlayerEvent, FfiPlayerSnapshot,
    FfiPlayerStatus, FfiRepeatMode, FfiResamplerKind, FfiRouteChangeReason, FfiStretchBackendKind,
    FfiTimeControlStatus, FfiTimeRange, FfiTotalBytesSource, FfiTrackFailureKind, FfiTrackStatus,
    FfiTransition, FfiVariant, duration_to_seconds,
};
