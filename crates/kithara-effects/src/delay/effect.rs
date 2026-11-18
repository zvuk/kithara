use std::{
    f64::consts::FRAC_1_SQRT_2,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_dsp::filter::{Response, SvfDesign};
use num_traits::{AsPrimitive, ToPrimitive};

use super::{
    CombError, DelayTime, Feedback, FeedbackDelayConfig, FeedbackHighPass, comb::FeedbackComb,
};
use crate::{DeckEffect, FxParamChange, FxParamDescriptor, consts};

/// Stereo deck FX delay: unfiltered first echo, then a Butterworth high-pass on each repeat.
/// Time/capacity count frames; time changes cross over 20 ms.
/// Retunes keep history and requested controls; ring-out ends the tail.
#[derive(Debug)]
pub struct FeedbackDelay {
    comb: FeedbackComb<{ consts::STEREO.get() }>,
    parameters: [FxParamDescriptor; 3],
    high_pass: FeedbackHighPass,
    sample_rate: NonZeroU32,
}

impl FeedbackDelay {
    /// Prepares the delay `config` asks for, its history taken from `pools`.
    ///
    /// # Errors
    /// [`CombError::Delay`] when the starting time exceeds the capacity,
    /// [`CombError::Pool`] when the history does not fit `pools`.
    ///
    /// # Panics
    /// If an internally declared parameter domain violates its checked bounds.
    pub fn new<S: HasPool<f32>>(
        config: FeedbackDelayConfig,
        pools: &PoolRegion<S>,
    ) -> Result<Self, CombError> {
        use crate::{
            FxParamMapping::{Linear, Log},
            FxParamUnit::{Frames, Hertz, Ratio},
        };

        let sample_rate = config.sample_rate();
        let capacity: f64 = config.capacity().get().as_();
        let parameters = [
            (
                f64::from(DelayTime::MIN)..=capacity.min(f64::from(DelayTime::MAX)),
                Frames,
                Linear,
            ),
            (
                f64::from(f32::from(Feedback::MIN))..=f64::from(f32::from(Feedback::MAX)),
                Ratio,
                Linear,
            ),
            (
                f64::from(f32::from(FeedbackHighPass::MIN))
                    ..=f64::from(f32::from(FeedbackHighPass::MAX)),
                Hertz,
                Log,
            ),
        ]
        .map(|(range, unit, mapping)| {
            // Nonzero capacity and the finite ranged bounds prove each domain valid.
            FxParamDescriptor::checked(range, unit, Some(mapping))
                .expect("prepared parameter domain")
        });
        let comb = FeedbackComb::new(
            config.capacity(),
            f64::from(config.time()),
            cross_frames(sample_rate),
            high_pass(sample_rate, config.high_pass()),
            f32::from(config.feedback()),
            pools,
        )?;
        Ok(Self {
            comb,
            parameters,
            sample_rate,
            high_pass: config.high_pass(),
        })
    }

    /// Requested cutoff, retained when the design saturates at a lower rate.
    #[must_use]
    pub fn high_pass(&self) -> FeedbackHighPass {
        self.high_pass
    }

    fn tune(&mut self, cutoff: FeedbackHighPass) {
        self.comb.retune(high_pass(self.sample_rate, cutoff));
        self.high_pass = cutoff;
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

    fn parameters(&self) -> &[FxParamDescriptor] {
        &self.parameters
    }

    fn set(&mut self, change: FxParamChange) {
        match change.index {
            0 => {
                if let Ok(delay) = self.comb.tap(change.value) {
                    self.comb.retime(delay);
                }
            }
            1 => {
                if let Some(feedback) = change.value.to_f32().and_then(Feedback::checked) {
                    self.comb.set_gain(f32::from(feedback));
                }
            }
            2 => {
                if let Some(cutoff) = change.value.to_f32().and_then(FeedbackHighPass::checked) {
                    self.tune(cutoff);
                }
            }
            _ => {}
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
    /// `sample_rate`; only the design cutoff saturates at the DSP ceiling.
    /// A cross already past its new length ends here.
    fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        if sample_rate == self.sample_rate {
            return;
        }
        self.sample_rate = sample_rate;
        self.comb.set_cross(cross_frames(sample_rate));
        self.tune(self.high_pass);
    }
}

fn cross_frames(sample_rate: NonZeroU32) -> NonZeroUsize {
    (f64::from(sample_rate.get()) * consts::DELAY_RAMP_SECONDS)
        .round()
        .to_usize()
        .and_then(NonZeroUsize::new)
        .unwrap_or(NonZeroUsize::MIN)
}

fn high_pass(sample_rate: NonZeroU32, cutoff: FeedbackHighPass) -> SvfDesign {
    let cutoff = f64::from(f32::from(cutoff)).min(SvfDesign::max_cutoff(sample_rate));
    // The positive ranged cutoff, DSP ceiling and fixed finite Q satisfy every design check.
    SvfDesign::new(sample_rate, cutoff, FRAC_1_SQRT_2, Response::HighPass)
        .expect("bounded Butterworth design")
}
