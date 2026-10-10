use num_traits::ToPrimitive;

use super::super::ElasticError;

/// One exact source-span to output-span rendering request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct ElasticRequest {
    /// Number of output frames filled by this request.
    #[field(get, copy)]
    output_frames: usize,
    /// Source-frame advance represented by the emitted output interval.
    #[field(get, copy)]
    pub(super) output_source_frames: usize,
    /// Number of source frames consumed by this request.
    #[field(get, copy)]
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
            output_source_frames: source_frames,
        })
    }

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
