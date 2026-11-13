use std::{
    f64::consts::FRAC_1_SQRT_2,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_dsp::{
    delay::{CombError, FeedbackComb},
    filter::{Coefficients, FilterError, Hertz, Type},
};
use num_traits::ToPrimitive;

use super::{DelayParam, FeedbackDelayConfig, FeedbackHighPass};
use crate::{DeckEffect, FxParamChange, consts};

/// Stereo feedback delay a deck FX unit holds.
///
/// It runs a [`FeedbackComb`] whose loop section is a Butterworth high-pass
/// at the cutoff: the first echo is the input unfiltered and every repeat
/// passes the high-pass once more. The delay time is the comb's delay in
/// frames, the feedback its gain, and a time change crosses over 20 ms. The
/// tail counts as spent once the comb has rung out. The delay time and the
/// capacity count frames, so a new sample rate keeps them and retunes the
/// high-pass and the cross length.
#[derive(Debug)]
pub struct FeedbackDelay {
    comb: FeedbackComb<{ consts::STEREO.get() }>,
    high_pass: FeedbackHighPass,
    sample_rate: NonZeroU32,
}

impl FeedbackDelay {
    /// Prepares the comb `config` asks for, its history taken from `pools`.
    ///
    /// # Errors
    /// [`CombError::Delay`] when the starting time exceeds the capacity,
    /// [`CombError::Pool`] when the history does not fit `pools`,
    /// [`CombError::Filter`] when the starting cutoff does not fit under
    /// Nyquist or the platform refuses the filter.
    pub fn new<S: HasPool<f32>>(
        config: FeedbackDelayConfig,
        pools: &PoolRegion<S>,
    ) -> Result<Self, CombError> {
        let sample_rate = config.sample_rate();
        let comb = FeedbackComb::new(
            config.capacity(),
            f64::from(config.time()),
            cross_frames(sample_rate),
            high_pass(sample_rate, config.high_pass())?,
            f32::from(config.feedback()),
            pools,
        )?;
        Ok(Self {
            comb,
            sample_rate,
            high_pass: config.high_pass(),
        })
    }

    /// Tunes the feedback high-pass to `cutoff` at the current sample rate.
    fn tune(&mut self, cutoff: FeedbackHighPass) -> Result<(), FilterError> {
        self.comb.retune(high_pass(self.sample_rate, cutoff)?)?;
        self.high_pass = cutoff;
        Ok(())
    }
}

impl DeckEffect for FeedbackDelay {
    fn process(&mut self, input: [&[f32]; 2], mut wet: [&mut [f32]; 2]) {
        let frames = input
            .iter()
            .map(|plane| plane.len())
            .chain(wet.iter().map(|plane| plane.len()))
            .min()
            .unwrap_or(0);
        if self.comb.process(&input, &mut wet, 0..frames).is_err() {
            for plane in &mut wet {
                plane.fill(0.0);
            }
        }
    }

    fn accepts(&self, change: &FxParamChange) -> bool {
        let FxParamChange::Delay(param) = change;
        match param {
            DelayParam::Time(time) => self.comb.tap(f64::from(*time)).is_ok(),
            DelayParam::Feedback(_) => true,
            DelayParam::HighPass(cutoff) => high_pass(self.sample_rate, *cutoff).is_ok(),
        }
    }

    fn set(&mut self, change: FxParamChange) {
        let FxParamChange::Delay(param) = change;
        match param {
            DelayParam::Time(time) => {
                if let Ok(delay) = self.comb.tap(f64::from(time)) {
                    self.comb.retime(delay);
                }
            }
            DelayParam::Feedback(feedback) => self.comb.set_gain(f32::from(feedback)),
            DelayParam::HighPass(cutoff) => {
                self.tune(cutoff).ok();
            }
        }
    }

    delegate::delegate! {
        to self.comb {
            /// Silences the history and the filter and lands any time change; the
            /// parameters set so far stay.
            fn reset(&mut self);
            #[call(rung_out)]
            fn tail_exhausted(&self) -> bool;
        }
    }

    /// Keeps the history, the delay time in frames and the feedback, and
    /// retunes the high-pass, the cross length and the tail window to
    /// `sample_rate`; a cutoff at or past its Nyquist keeps the previous
    /// tuning. A cross already past its new length ends here.
    fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        if sample_rate == self.sample_rate {
            return;
        }
        self.sample_rate = sample_rate;
        self.comb.set_cross(cross_frames(sample_rate));
        self.tune(self.high_pass).ok();
    }
}

/// The 20 ms a time change crosses over, in frames at `sample_rate`; at
/// least one.
fn cross_frames(sample_rate: NonZeroU32) -> NonZeroUsize {
    (f64::from(sample_rate.get()) * consts::DELAY_RAMP_SECONDS)
        .round()
        .to_usize()
        .and_then(NonZeroUsize::new)
        .unwrap_or(NonZeroUsize::MIN)
}

/// The Butterworth high-pass at `cutoff` for `sample_rate`.
fn high_pass(
    sample_rate: NonZeroU32,
    cutoff: FeedbackHighPass,
) -> Result<Coefficients<f64>, FilterError> {
    let hertz = |value: f64| Hertz::<f64>::from_hz(value).map_err(|_| FilterError::Parameters);
    Coefficients::from_params(
        Type::HighPass,
        hertz(f64::from(sample_rate.get()))?,
        hertz(f64::from(f32::from(cutoff)))?,
        FRAC_1_SQRT_2,
    )
    .map_err(|_| FilterError::Parameters)
}
