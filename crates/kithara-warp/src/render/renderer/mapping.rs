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
        let Some(plan) = self.plan.as_ref() else {
            return Ok(nominal);
        };
        let mut position = fraction(self.trajectory.at_offset(start, rate, 0)?);
        let mut remaining = fraction(nominal)
            .sub(position)
            .ok_or(ElasticError::SampleCountOverflow)?;
        loop {
            let source = u64::try_from(position.numerator / position.denominator.get())
                .map_err(|_| ElasticError::SampleCountOverflow)?;
            let region = plan.region_at(source);
            let correction = Fraction::correction(region.correction())
                .ok_or(ElasticError::SampleCountOverflow)?;
            if region.end() == u64::MAX {
                let position = remaining
                    .div(correction)
                    .and_then(|advance| position.add(advance))
                    .ok_or(ElasticError::SampleCountOverflow)?;
                return Ok((position.numerator, position.denominator));
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
                let position = remaining
                    .div(correction)
                    .and_then(|advance| position.add(advance))
                    .ok_or(ElasticError::SampleCountOverflow)?;
                return Ok((position.numerator, position.denominator));
            }
            remaining = remaining
                .sub(available)
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
        let Some(plan) = self.plan.as_ref() else {
            return Ok(nominal);
        };
        let position = fraction(
            nominal
                .source_ratio_at(0)
                .ok_or(ElasticError::SampleCountOverflow)?,
        );
        let region = plan.region_at(nominal.start());
        let correction =
            Fraction::correction(region.correction()).ok_or(ElasticError::SampleCountOverflow)?;
        let advance = |frame| {
            fraction(nominal.source_ratio_at(frame)?)
                .sub(position)?
                .div(correction)?
                .add(position)
        };
        let mut lower = 0;
        let mut upper = nominal.output_frames();
        while lower < upper {
            let middle = lower + (upper - lower).div_ceil(2);
            let endpoint = advance(middle).ok_or(ElasticError::SampleCountOverflow)?;
            let whole = endpoint.numerator / endpoint.denominator.get();
            if whole < u128::from(region.end())
                || (whole == u128::from(region.end())
                    && endpoint
                        .numerator
                        .is_multiple_of(endpoint.denominator.get()))
            {
                lower = middle;
            } else {
                upper = middle - 1;
            }
        }
        let first = if lower == 0 {
            fraction(self.mapped_position(start, rate, 1)?).sub(position)
        } else {
            advance(1).and_then(|next| next.sub(position))
        }
        .ok_or(ElasticError::SampleCountOverflow)?;
        let next = if lower > 1 {
            advance(2).and_then(|next| advance(1).and_then(|first| next.sub(first)))
        } else {
            Some(first)
        }
        .ok_or(ElasticError::SampleCountOverflow)?;
        let common = position
            .common(first)
            .and_then(|denominator| {
                Fraction {
                    numerator: 0,
                    denominator: NonZeroU128::new(denominator)?,
                }
                .common(next)
            })
            .ok_or(ElasticError::SampleCountOverflow)?;
        let step = first.at(common).ok_or(ElasticError::SampleCountOverflow)?;
        let change = next
            .at(common)
            .and_then(|next| i128::try_from(next).ok())
            .and_then(|next| {
                i128::try_from(step)
                    .ok()
                    .and_then(|step| next.checked_sub(step))
            })
            .ok_or(ElasticError::SampleCountOverflow)?;
        SourceSpan::try_from((
            position
                .at(common)
                .ok_or(ElasticError::SampleCountOverflow)?,
            step,
            change,
            NonZeroU128::new(common).ok_or(ElasticError::SampleCountOverflow)?,
            rate,
            lower.max(1),
        ))
        .ok()
        .ok_or(ElasticError::SampleCountOverflow)
    }
}
