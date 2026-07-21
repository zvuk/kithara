use kithara_bufpool::BudgetExhausted;
use kithara_stretch::ElasticError;

use crate::SourceRangeError;

/// Failure while preparing, reading, relocating, or rendering exact-span PCM.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ElasticReaderError {
    #[error("bound playback requires canonical decoded source ranges")]
    SourceUnavailable,
    #[error("elastic reader source format does not match the output stream")]
    FormatMismatch,
    #[error("elastic reader source channels exceed the output channel count")]
    UnsupportedChannelLayout,
    #[error("elastic reader frame arithmetic overflowed")]
    FrameOverflow,
    #[error("elastic reader buffer budget is exhausted")]
    BufferBudget(#[from] BudgetExhausted),
    #[error(transparent)]
    Source(#[from] SourceRangeError),
    #[error(transparent)]
    Elastic(#[from] ElasticError),
    #[error("decoded source ended before elastic preparation completed")]
    SourceEnded,
    #[error("the elastic backend does not support reverse input")]
    ReverseUnsupported,
    #[error("elastic source range is outside the prepared fetch window")]
    FetchWindowMismatch,
    #[error("another elastic relocation is still pending")]
    RelocationPending,
    #[error("elastic output channel layout does not match its preparation")]
    OutputChannelMismatch,
    #[error("elastic history was prepared for a different playback direction")]
    DirectionMismatch,
    #[error("elastic source read failed")]
    SourceReadFailed,
    #[error("elastic source window missed its render deadline")]
    SourceWindowDeadlineMissed,
    #[error("elastic relocation is not ready")]
    RelocationNotReady,
    #[error("elastic relocation could not be primed")]
    RelocationPreparationFailed,
    #[error("elastic reader configuration field `{field}` must be positive")]
    InvalidConfig { field: &'static str },
}
