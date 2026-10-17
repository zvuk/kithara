use std::num::{NonZeroU32, NonZeroU128};

use kithara_bufpool::HasPool;
use kithara_signal::SourceSpan;
use kithara_stretch::ElasticError;

use super::{super::trajectory::Fraction, core::WarpRenderer};

fn fraction(position: (u128, NonZeroU128)) -> Fraction {
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
    pub(in crate::render) fn projection_stages(&self) -> Result<usize, ElasticError> {
        let (minimum, maximum) = self.plan.as_ref().map_or((1.0, 1.0), |plan| {
            plan.segments()
                .iter()
                .fold((1.0_f64, 1.0_f64), |(minimum, maximum), segment| {
                    (
                        minimum.min(segment.ratio_correction()),
                        maximum.max(segment.ratio_correction()),
                    )
                })
        });
        Ok(self
            .trajectory
            .projection_stages(minimum)?
            .max(self.trajectory.projection_stages(maximum)?))
    }

    pub(in crate::render) fn mapped_position(
        &self,
        start: u64,
        rate: NonZeroU32,
        offset: u64,
    ) -> Result<(u128, NonZeroU128), ElasticError> {
        let nominal = self.trajectory.at_offset(start, rate, offset)?;
        self.map_nominal_position(start, nominal)
    }

    pub(in crate::render) fn map_nominal_position(
        &self,
        start: u64,
        nominal: (u128, NonZeroU128),
    ) -> Result<(u128, NonZeroU128), ElasticError> {
        if self.plan.is_none() {
            return Ok(nominal);
        }
        let (pivot, position, correction, _end) = self.mapped_region(start, nominal)?;
        let mapped = fraction(nominal)
            .sub(pivot)
            .and_then(|advance| advance.div(correction))
            .and_then(|advance| position.add(advance))
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok((mapped.numerator, mapped.denominator))
    }

    fn mapped_region(
        &self,
        start: u64,
        nominal: (u128, NonZeroU128),
    ) -> Result<(Fraction, Fraction, Fraction, u64), ElasticError> {
        let plan = self.plan.as_ref().ok_or(ElasticError::EnginePreparation(
            "region mapping requires its accepted plan",
        ))?;
        let mut position = fraction(self.trajectory.mapping_origin(start)?);
        let mut pivot = position;
        let mut remaining = fraction(nominal)
            .sub(pivot)
            .ok_or(ElasticError::SampleCountOverflow)?;
        loop {
            let source = u64::try_from(position.numerator / position.denominator.get())
                .map_err(|_| ElasticError::SampleCountOverflow)?;
            let region = plan.region_at(source);
            let correction = Fraction::correction(region.correction())
                .ok_or(ElasticError::SampleCountOverflow)?;
            if region.end() == u64::MAX {
                return Ok((pivot, position, correction, region.end()));
            }
            let end = Fraction {
                numerator: u128::from(region.end()),
                denominator: NonZeroU128::MIN,
            };
            let available = end
                .sub(position)
                .and_then(|distance| distance.mul(correction))
                .ok_or(ElasticError::SampleCountOverflow)?;
            let common = remaining
                .common(available)
                .ok_or(ElasticError::SampleCountOverflow)?;
            if remaining
                .at(common)
                .zip(available.at(common))
                .is_some_and(|(remaining, available)| remaining <= available)
            {
                return Ok((pivot, position, correction, region.end()));
            }
            remaining = remaining
                .sub(available)
                .ok_or(ElasticError::SampleCountOverflow)?;
            pivot = pivot
                .add(available)
                .ok_or(ElasticError::SampleCountOverflow)?;
            position = end;
        }
    }

    pub(in crate::render) fn mapped_frame_speed(
        &self,
        start: u64,
        rate: NonZeroU32,
        frame: u64,
    ) -> Result<f64, ElasticError> {
        let first = fraction(self.mapped_position(start, rate, frame)?);
        let next = fraction(
            self.mapped_position(
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

    fn region_span(
        &self,
        start: u64,
        nominal: SourceSpan,
    ) -> Result<(SourceSpan, u64), ElasticError> {
        let nominal_start = nominal
            .source_ratio_at(0)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let (pivot, position, correction, end) = self.mapped_region(start, nominal_start)?;
        let mapped = nominal
            .scaled_from(
                (pivot.numerator, pivot.denominator),
                (position.numerator, position.denominator),
                (
                    correction.denominator.get(),
                    NonZeroU128::new(correction.numerator)
                        .ok_or(ElasticError::SampleCountOverflow)?,
                ),
            )
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok((mapped, end))
    }

    fn span_within_region(
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
        let nominal = self.trajectory.span(start, rate, frames)?;
        if self.plan.is_none() {
            return Ok(nominal);
        }
        let (mapped, end) = self.region_span(start, nominal)?;
        if let Some(span) = Self::span_within_region(mapped, end)? {
            return Ok(span);
        }
        let first = fraction(self.mapped_position(start, rate, 0)?);
        let advance = fraction(self.mapped_position(start, rate, 1)?)
            .sub(first)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let common = first
            .common(advance)
            .ok_or(ElasticError::SampleCountOverflow)?;
        SourceSpan::try_from((
            first.at(common).ok_or(ElasticError::SampleCountOverflow)?,
            advance
                .at(common)
                .ok_or(ElasticError::SampleCountOverflow)?,
            NonZeroU128::new(common).ok_or(ElasticError::SampleCountOverflow)?,
            rate,
            1,
        ))
        .ok()
        .ok_or(ElasticError::SampleCountOverflow)
    }
}
