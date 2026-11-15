use kithara_bufpool::HasPool;
use kithara_signal::SourceSpan;
use kithara_stretch::ElasticError;

use super::core::WarpRenderer;

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(in crate::render) fn render_varispeed(
        &mut self,
        span: SourceSpan,
    ) -> Result<(), ElasticError> {
        let channels = usize::from(self.spec.channels);
        let frames =
            usize::try_from(span.output_frames()).map_err(|_| ElasticError::SampleCountOverflow)?;
        let samples = frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let scratch = self.scratch.as_mut().ok_or(ElasticError::PoolCapacity)?;
        if samples > scratch.capacity() {
            return Err(ElasticError::PoolCapacity);
        }
        scratch
            .ensure_len(samples)
            .map_err(|_| ElasticError::PoolCapacity)?;
        scratch.truncate(samples);
        let resident = self.residency.as_ref().ok_or(ElasticError::PoolCapacity)?;
        let mut steps = span
            .source_steps()
            .ok_or(ElasticError::SampleCountOverflow)?;
        for output in scratch.chunks_exact_mut(channels) {
            let step = steps.next_step().ok_or(ElasticError::SampleCountOverflow)?;
            let source =
                i64::try_from(step.frame).map_err(|_| ElasticError::SampleCountOverflow)?;
            super::super::source_sample::source_frame(
                resident,
                (source, step.fraction),
                step.advance,
                self.terminal_source_end,
                output,
            )?;
        }
        Ok(())
    }
}
