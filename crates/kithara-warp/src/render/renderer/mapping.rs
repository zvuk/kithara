use std::num::{NonZeroU32, NonZeroU128};

use kithara_bufpool::HasPool;
use kithara_signal::SourceSpan;
use kithara_stretch::ElasticError;

use super::{super::trajectory::Fraction, core::WarpRenderer};

pub(super) fn fraction(position: (u128, NonZeroU128)) -> Fraction {
    Fraction {
        numerator: position.0,
        denominator: position.1,
    }
}

pub(super) fn source_demand(span: SourceSpan, start: u64) -> Result<usize, ElasticError> {
    let (numerator, denominator) = span
        .source_ratio_at(span.output_frames())
        .ok_or(ElasticError::SampleCountOverflow)?;
    u64::try_from(numerator.div_ceil(denominator.get()))
        .ok()
        .map(|end| end.saturating_sub(start))
        .and_then(|frames| usize::try_from(frames).ok())
        .ok_or(ElasticError::SampleCountOverflow)
}

pub(in crate::render) fn span_speed(span: SourceSpan, frame: u64) -> Result<f64, ElasticError> {
    let first = span.source_ratio_at(frame).map(fraction);
    let next = frame
        .checked_add(1)
        .and_then(|frame| span.source_ratio_at(frame))
        .map(fraction);
    first
        .zip(next)
        .and_then(|(first, next)| next.sub(first))
        .and_then(Fraction::as_f64)
        .ok_or(ElasticError::SampleCountOverflow)
}

impl<S: HasPool<f32>> WarpRenderer<S> {
    pub(in crate::render) fn mapped_frame_speed(
        &self,
        start: u64,
        rate: NonZeroU32,
        frame: u64,
    ) -> Result<f64, ElasticError> {
        let first = fraction(self.trajectory.at_offset(start, rate, frame)?);
        let next = fraction(
            self.trajectory.at_offset(
                start,
                rate,
                frame
                    .checked_add(1)
                    .ok_or(ElasticError::SampleCountOverflow)?,
            )?,
        );
        next.sub(first)
            .and_then(Fraction::as_f64)
            .ok_or(ElasticError::SampleCountOverflow)
    }

    pub(super) fn span_until(
        mapped: SourceSpan,
        end: u64,
    ) -> Result<Option<SourceSpan>, ElasticError> {
        let mut lower = 0;
        let mut upper = mapped.output_frames();
        while lower < upper {
            let middle = lower + (upper - lower).div_ceil(2);
            let endpoint = fraction(
                mapped
                    .source_ratio_at(middle)
                    .ok_or(ElasticError::SampleCountOverflow)?,
            );
            let whole = endpoint.numerator / endpoint.denominator.get();
            if whole < u128::from(end)
                || (whole == u128::from(end)
                    && endpoint
                        .numerator
                        .is_multiple_of(endpoint.denominator.get()))
            {
                lower = middle;
            } else {
                upper = middle - 1;
            }
        }
        if lower > 0 {
            return mapped
                .for_output_range(0..lower)
                .map(Some)
                .ok_or(ElasticError::SampleCountOverflow);
        }
        Ok(None)
    }

    pub(in crate::render) fn mapping_span(
        &self,
        start: u64,
        rate: NonZeroU32,
        frames: usize,
    ) -> Result<SourceSpan, ElasticError> {
        if !self.requires_staging() {
            return self.trajectory.unity_span(start, rate, frames);
        }
        self.trajectory.span(start, rate, frames)
    }
}
