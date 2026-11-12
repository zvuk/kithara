use num_traits::ToPrimitive;

use super::{
    approach::Approach,
    mapping::{Mapping, Rational},
    ratio::Ratio,
};

/// One output frame's exact source coordinate: the decoded-source frame it
/// falls in, the fraction past that frame, and the source advance to the next
/// output frame.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct SourceStep {
    pub frame: u64,
    pub fraction: f64,
    pub advance: f64,
}

/// The source coordinate of each output frame of a span, in output order.
#[derive(Clone, Copy, Debug)]
pub struct SourceSteps(State);

#[derive(Clone, Copy, Debug)]
enum State {
    Rational(RationalSteps),
    Approach(ApproachSteps),
}

/// The numerator over a fixed denominator advances by addition alone, so a
/// frame costs no division while the step holds.
#[derive(Clone, Copy, Debug)]
struct RationalSteps {
    whole: u128,
    rest: u128,
    denominator: u128,
    step: u128,
    step_whole: u128,
    step_rest: u128,
    change: i128,
}

#[derive(Clone, Copy, Debug)]
struct ApproachSteps {
    mapping: Approach,
    frame: u64,
    current: Ratio,
}

impl SourceSteps {
    pub(super) fn new(mapping: Mapping) -> Option<Self> {
        Some(Self(match mapping {
            Mapping::Rational(rational) => State::Rational(RationalSteps::new(rational)),
            Mapping::Approach(mapping) => State::Approach(ApproachSteps {
                mapping,
                frame: 0,
                current: mapping.ratio_at(0)?,
            }),
        }))
    }

    /// The next output frame's coordinate, or `None` when it is not
    /// representable.
    pub fn next_step(&mut self) -> Option<SourceStep> {
        match &mut self.0 {
            State::Rational(steps) => steps.next_step(),
            State::Approach(steps) => steps.next_step(),
        }
    }
}

impl RationalSteps {
    fn new(
        Rational {
            numerator,
            denominator,
            step,
            step_change,
        }: Rational,
    ) -> Self {
        let denominator = denominator.get();
        Self {
            whole: numerator / denominator,
            rest: numerator % denominator,
            denominator,
            step,
            step_whole: step / denominator,
            step_rest: step % denominator,
            change: step_change,
        }
    }

    fn next_step(&mut self) -> Option<SourceStep> {
        let scale = self.denominator.to_f64()?;
        let item = SourceStep {
            frame: u64::try_from(self.whole).ok()?,
            fraction: self.rest.to_f64()? / scale,
            advance: self.step.to_f64()? / scale,
        };
        self.whole = self.whole.checked_add(self.step_whole)?;
        self.rest += self.step_rest;
        if self.rest >= self.denominator {
            self.rest -= self.denominator;
            self.whole = self.whole.checked_add(1)?;
        }
        if self.change != 0 {
            self.accelerate()?;
        }
        Some(item)
    }

    /// A ramp changes the step by a constant every frame.
    fn accelerate(&mut self) -> Option<()> {
        let change = self.change.unsigned_abs();
        self.step = if self.change < 0 {
            self.step.checked_sub(change)?
        } else {
            self.step.checked_add(change)?
        };
        self.step_whole = self.step / self.denominator;
        self.step_rest = self.step % self.denominator;
        Some(())
    }
}

impl ApproachSteps {
    fn next_step(&mut self) -> Option<SourceStep> {
        self.frame = self.frame.checked_add(1)?;
        let next = self.mapping.ratio_at(self.frame)?;
        let advance = next.sub(self.current)?;
        let current = self.current;
        let item = SourceStep {
            frame: current.source_frame()?,
            fraction: (current.numerator % current.denominator.get()).to_f64()?
                / current.denominator.get().to_f64()?,
            advance: advance.numerator.to_f64()? / advance.denominator.get().to_f64()?,
        };
        self.current = next;
        Some(item)
    }
}
