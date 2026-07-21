/// Error from elastic planning, engine preparation, or processing.
#[non_exhaustive]
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ElasticError {
    /// The sample rate was zero.
    #[error("elastic sample rate must be positive")]
    InvalidSampleRate,
    /// The channel count was zero.
    #[error("elastic channel count must be positive")]
    InvalidChannelCount,
    /// The channel count cannot be represented by the backend.
    #[error("elastic channel count {0} exceeds the backend limit")]
    ChannelCountOutOfRange(usize),
    /// The prepared source-frame limit was zero.
    #[error("elastic source-frame limit must be positive")]
    InvalidSourceFrameLimit,
    /// The prepared output-frame limit was zero.
    #[error("elastic output-frame limit must be positive")]
    InvalidOutputFrameLimit,
    /// The source-frame limit cannot be represented by the backend.
    #[error("elastic source-frame limit {0} exceeds the backend limit")]
    SourceFrameLimitOutOfRange(usize),
    /// The output-frame limit cannot be represented by the backend.
    #[error("elastic output-frame limit {0} exceeds the backend limit")]
    OutputFrameLimitOutOfRange(usize),
    /// The request contained no source frames.
    #[error("elastic request must contain source frames")]
    EmptySource,
    /// The request contained no output frames.
    #[error("elastic request must contain output frames")]
    EmptyOutput,
    /// The request exceeded the prepared source-frame limit.
    #[error("elastic request has {frames} source frames; prepared limit is {limit}")]
    SourceFrameLimit { frames: usize, limit: usize },
    /// The request exceeded the prepared output-frame limit.
    #[error("elastic request has {frames} output frames; prepared limit is {limit}")]
    OutputFrameLimit { frames: usize, limit: usize },
    /// The source block length did not match the request and channel count.
    #[error("elastic source block has {actual} samples; expected {expected}")]
    SourceSampleCount { actual: usize, expected: usize },
    /// The output block length did not match the request and channel count.
    #[error("elastic output block has {actual} samples; expected {expected}")]
    OutputSampleCount { actual: usize, expected: usize },
    /// The source-to-output frame rate was outside the supported envelope.
    #[error(
        "elastic request rate is outside the supported envelope: {source_frames} source frames for {output_frames} output frames"
    )]
    RateOutsideEnvelope {
        source_frames: usize,
        output_frames: usize,
    },
    /// A continuous source-to-output rate was invalid or unsupported.
    #[error("elastic source rate {0} is outside the supported envelope")]
    InvalidRate(f64),
    /// The priming history did not match the backend latency and channel count.
    #[error("elastic priming history has {actual} samples; expected {expected}")]
    HistorySampleCount { actual: usize, expected: usize },
    /// A priming request did not produce exactly the declared output latency.
    #[error("elastic warmup has {actual} output frames; expected {expected}")]
    WarmupOutputFrameCount { actual: usize, expected: usize },
    /// A block sample count overflowed the platform index type.
    #[error("elastic block sample count overflow")]
    SampleCountOverflow,
    /// A continuous source coordinate was not finite or representable.
    #[error("elastic source coordinate {0} is not finite or representable")]
    InvalidSourceCoordinate(f64),
    /// A continuous span did not advance through the source.
    #[error("elastic source span must advance")]
    StationarySourceSpan,
    /// Exact-span planning received no source spans.
    #[error("elastic span plan must contain at least one span")]
    EmptySpanPlan,
    /// Exact-span planning exceeded its fixed real-time span capacity.
    #[error("elastic span plan exceeds its fixed capacity of {limit} spans")]
    SpanLimit { limit: usize },
    /// Consecutive continuous source spans did not share one boundary.
    #[error("elastic source path is discontinuous: expected {expected}, received {actual}")]
    DiscontinuousSource { expected: f64, actual: f64 },
    /// Consecutive source spans changed playback direction inside one block.
    #[error("elastic source path changes direction inside one render block")]
    SourceDirectionChange,
    /// Exact-span frame or cursor arithmetic overflowed.
    #[error("elastic span arithmetic overflowed")]
    SpanArithmeticOverflow,
    /// A source cursor was too far from the requested continuous source path.
    #[error("elastic phase error {error} exceeds the continuous correction limit {limit}")]
    PhaseDiscontinuity { error: f64, limit: f64 },
    /// The backend rate envelope had no headroom for continuous correction.
    #[error("elastic phase error {error} cannot be corrected inside the backend rate envelope")]
    PhaseCorrectionUnavailable { error: f64 },
}
