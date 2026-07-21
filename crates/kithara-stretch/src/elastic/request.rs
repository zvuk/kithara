#[cfg(feature = "stretch-signalsmith")]
use num_traits::ToPrimitive;

use super::ElasticError;

/// One exact source-span to output-span rendering request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ElasticRequest {
    output_frames: usize,
    source_frames: usize,
}

impl ElasticRequest {
    /// Creates a non-empty numeric frame-span request.
    /// # Errors
    /// Returns [`ElasticError`] when either frame span is empty.
    pub const fn new(source_frames: usize, output_frames: usize) -> Result<Self, ElasticError> {
        if source_frames == 0 {
            return Err(ElasticError::EmptySource);
        }
        if output_frames == 0 {
            return Err(ElasticError::EmptyOutput);
        }
        Ok(Self {
            output_frames,
            source_frames,
        })
    }

    /// Number of output frames filled by this request.
    #[must_use]
    pub const fn output_frames(self) -> usize {
        self.output_frames
    }

    /// Number of source frames consumed by this request.
    #[must_use]
    pub const fn source_frames(self) -> usize {
        self.source_frames
    }

    #[cfg(feature = "stretch-signalsmith")]
    pub(crate) fn source_frames_per_output(self) -> Result<f64, ElasticError> {
        let source_frames = self
            .source_frames
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let output_frames = self
            .output_frames
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok(source_frames / output_frames)
    }
}
