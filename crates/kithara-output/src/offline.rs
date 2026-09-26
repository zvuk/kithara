use std::{error::Error as StdError, ops::Range};

use bon::Builder;
use kithara_platform::CancelToken;
use kithara_signal::AudioSpec;
use thiserror::Error;

/// One exact, finite output-frame range to render.
#[derive(Clone, Debug, Builder)]
#[non_exhaustive]
pub struct OfflineRenderRequest {
    /// Output signal format expected by the caller and sink.
    spec: AudioSpec,
    /// Absolute half-open output-frame range.
    frames: Range<u64>,
}

impl OfflineRenderRequest {
    /// Exact number of requested frames.
    ///
    /// # Errors
    /// Returns an invalid-range error when the end precedes the start.
    pub fn frame_count(&self) -> Result<u64, OfflineRenderError> {
        self.frames
            .end
            .checked_sub(self.frames.start)
            .ok_or(OfflineRenderError::InvalidRange {
                start: self.frames.start,
                end: self.frames.end,
            })
    }

    /// Absolute half-open output-frame range.
    #[must_use]
    pub const fn frames(&self) -> &Range<u64> {
        &self.frames
    }

    /// Expected output signal format.
    #[must_use]
    pub const fn spec(&self) -> AudioSpec {
        self.spec
    }
}

/// Receiver for rendered interleaved `f32` blocks.
pub trait RenderSink {
    /// Consume one complete block.
    ///
    /// # Errors
    /// Returns a destination or encoding failure. The renderer stops without
    /// sending later frames.
    fn write(&mut self, samples: &[f32]) -> Result<(), RenderSinkError>;
}

/// Product protocol for one exact finite offline render.
pub trait OfflineRenderer {
    /// Drive the owned audio graph into `sink`.
    ///
    /// # Errors
    /// Returns before success on invalid range/specification, cancellation,
    /// backend failure, or sink failure.
    fn render(
        &mut self,
        request: &OfflineRenderRequest,
        cancel: &CancelToken,
        sink: &mut dyn RenderSink,
    ) -> Result<OfflineRenderReport, OfflineRenderError>;
}

/// Completed finite-render observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct OfflineRenderReport {
    /// Frames delivered to the sink.
    pub frames: u64,
}

impl OfflineRenderReport {
    /// Construct a successful report.
    #[must_use]
    pub const fn new(frames: u64) -> Self {
        Self { frames }
    }
}

/// Type-erased error returned by a render sink.
#[derive(Debug, Error)]
#[error("{source}")]
pub struct RenderSinkError {
    source: Box<dyn StdError + Send + Sync>,
}

impl RenderSinkError {
    /// Preserve a concrete sink failure behind the protocol boundary.
    #[must_use]
    pub fn new<E>(error: E) -> Self
    where
        E: StdError + Send + Sync + 'static,
    {
        Self {
            source: Box::new(error),
        }
    }
}

/// Failure category for one finite offline render.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OfflineRenderError {
    /// The selected Host session is not an offline renderer.
    #[error("Host session is not configured for offline rendering")]
    SessionModeUnavailable,
    /// The half-open frame range is reversed.
    #[error("offline render range {start}..{end} is invalid")]
    InvalidRange {
        /// Requested first frame.
        start: u64,
        /// Requested exclusive end frame.
        end: u64,
    },
    /// The renderer cannot rewind an already consumed timeline.
    #[error("offline render starts at frame {requested}, but the renderer is already at {current}")]
    RangeUnavailable {
        /// Requested first frame.
        requested: u64,
        /// Current renderer frame.
        current: u64,
    },
    /// Caller and renderer disagree on signal format.
    #[error("offline render expected {expected}, got {actual}")]
    SpecMismatch {
        /// Renderer-owned format.
        expected: AudioSpec,
        /// Request format.
        actual: AudioSpec,
    },
    /// Cancellation stopped the render before atomic publication.
    #[error("offline render cancelled after {rendered_frames} frames")]
    Cancelled {
        /// Frames already delivered to the sink.
        rendered_frames: u64,
    },
    /// Host/backend graph processing failed.
    #[error("offline render backend failed: {source}")]
    Backend {
        /// Backend failure.
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },
    /// Sink rejected a rendered block.
    #[error("offline render sink failed after {rendered_frames} frames: {source}")]
    Sink {
        /// Frames delivered before the failed block.
        rendered_frames: u64,
        /// Sink failure.
        #[source]
        source: RenderSinkError,
    },
}

impl OfflineRenderError {
    /// Wrap a concrete Host/backend failure.
    #[must_use]
    pub fn backend<E>(error: E) -> Self
    where
        E: StdError + Send + Sync + 'static,
    {
        Self::Backend {
            source: Box::new(error),
        }
    }

    /// Construct a sink failure with the delivered-frame count.
    #[must_use]
    pub fn sink(rendered_frames: u64, source: RenderSinkError) -> Self {
        Self::Sink {
            rendered_frames,
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::*;

    fn request(frames: Range<u64>) -> OfflineRenderRequest {
        OfflineRenderRequest::builder()
            .spec(AudioSpec::new(
                2,
                NonZeroU32::new(48_000).expect("test sample rate"),
            ))
            .frames(frames)
            .build()
    }

    #[kithara::test]
    fn finite_range_reports_its_exact_length() {
        assert!(matches!(request(17..42).frame_count(), Ok(25)));
    }

    #[kithara::test]
    fn reversed_range_is_rejected() {
        assert!(matches!(
            request(Range { start: 42, end: 17 }).frame_count(),
            Err(OfflineRenderError::InvalidRange { start: 42, end: 17 })
        ));
    }
}
