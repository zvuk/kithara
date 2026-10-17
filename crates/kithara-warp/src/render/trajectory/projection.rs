use std::{
    cmp::Ordering,
    num::{NonZeroU32, NonZeroU128},
};

use kithara_stretch::ElasticError;

use super::{Fraction, Phase, Trajectory};

impl Fraction {
    fn compare(self, other: Self) -> Ordering {
        let (mut first, mut first_denominator) = (self.numerator, self.denominator.get());
        let (mut second, mut second_denominator) = (other.numerator, other.denominator.get());
        let mut reversed = false;
        loop {
            let order = (first / first_denominator).cmp(&(second / second_denominator));
            if order != Ordering::Equal {
                return if reversed { order.reverse() } else { order };
            }
            let first_remainder = first % first_denominator;
            let second_remainder = second % second_denominator;
            if first_remainder == 0 || second_remainder == 0 {
                let order = first_remainder.cmp(&second_remainder);
                return if reversed { order.reverse() } else { order };
            }
            (first, first_denominator) = (first_denominator, first_remainder);
            (second, second_denominator) = (second_denominator, second_remainder);
            reversed = !reversed;
        }
    }
}

impl Trajectory {
    /// Aligns an observed source phase while preserving the retained analytic basis.
    ///
    /// # Errors
    /// Returns an error if translating the source basis is unrepresentable.
    pub fn reanchor(
        &mut self,
        start: u64,
        rate: NonZeroU32,
        position: (u128, NonZeroU128),
    ) -> Result<(), ElasticError> {
        let position =
            Fraction::new(position.0, position.1.get()).ok_or(ElasticError::SampleCountOverflow)?;
        let previous = self.at_offset(start, rate, 0)?;
        let previous = Fraction {
            numerator: previous.0,
            denominator: previous.1,
        };
        let origin = self
            .curve_origin
            .map(|(origin, elapsed)| {
                let translated = match position.compare(previous) {
                    Ordering::Less => previous.sub(position).and_then(|offset| origin.sub(offset)),
                    Ordering::Equal | Ordering::Greater => {
                        position.sub(previous).and_then(|offset| origin.add(offset))
                    }
                };
                translated
                    .map(|origin| (origin, elapsed))
                    .ok_or(ElasticError::SampleCountOverflow)
            })
            .transpose()?;
        self.position = Some(Phase::Exact(position));
        self.curve_origin = origin;
        Ok(())
    }

    /// Finds the first output boundary reaching an exact source endpoint.
    ///
    /// An inclusive limit bounds the search before the next accepted command.
    ///
    /// # Errors
    /// Returns an error if the source law or output range is unrepresentable.
    #[kithara_test_macros::hang_watchdog]
    pub fn first_at_or_after(
        &self,
        start: u64,
        rate: NonZeroU32,
        endpoint: (u128, NonZeroU128),
        limit: Option<u64>,
    ) -> Result<Option<u64>, ElasticError> {
        let endpoint = Fraction {
            numerator: endpoint.0,
            denominator: endpoint.1,
        };
        let reached = |position: (u128, NonZeroU128)| {
            Fraction {
                numerator: position.0,
                denominator: position.1,
            }
            .compare(endpoint)
                != Ordering::Less
        };
        if reached(self.at_offset(start, rate, 0)?) {
            return Ok(Some(0));
        }
        let maximum = limit.unwrap_or(u64::MAX - self.elapsed);
        let mut trajectory = self.clone();
        let mut cursor = 0;
        let mut upper = 1u64.min(maximum);
        while cursor < maximum {
            hang_tick!();
            let span =
                trajectory.source_span(start, rate, trajectory.output_limit(upper - cursor))?;
            let position = span
                .source_ratio_at(span.output_frames())
                .ok_or(ElasticError::SampleCountOverflow)?;
            if reached(position) {
                let mut lower = 0;
                let mut upper = span.output_frames();
                while upper - lower > 1 {
                    hang_tick!();
                    let middle = lower + (upper - lower) / 2;
                    let position = span
                        .source_ratio_at(middle)
                        .ok_or(ElasticError::SampleCountOverflow)?;
                    if reached(position) {
                        upper = middle;
                    } else {
                        lower = middle;
                    }
                    hang_reset!();
                }
                return cursor
                    .checked_add(upper)
                    .map(Some)
                    .ok_or(ElasticError::SampleCountOverflow);
            }
            trajectory.advance(span)?;
            cursor = cursor
                .checked_add(span.output_frames())
                .ok_or(ElasticError::SampleCountOverflow)?;
            if cursor == upper {
                upper = upper.saturating_mul(2).min(maximum);
            }
            hang_reset!();
        }
        Ok(None)
    }
}
