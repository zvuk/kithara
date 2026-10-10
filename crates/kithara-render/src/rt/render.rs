use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use kithara_dsp::param::{SmoothedParam, SmootherConfig};
use kithara_effects::{
    GainDb,
    eq::{EqConfig, EqLayout, StereoEq, generate_log_spaced_bands},
};

use super::processor::StreamShape;

/// A deck's output stage: the gain and equaliser every slot's mix passes through.
pub(crate) struct RenderPass {
    /// The deck's output gain, ramped to each new target from the frame it is set on.
    gain: SmoothedParam,
    /// The deck's equaliser, after its gain.
    eq: StereoEq,
    /// Set until the first range renders, which moves every ramp to its target at once.
    priming: bool,
}

impl RenderPass {
    /// A deck's gain runs from silence to unity.
    const GAIN_SPAN: f32 = 1.0;

    pub(crate) fn new<S>(
        pools: &PoolRegion<S>,
        shape: StreamShape,
        gain: f32,
        config: super::DeckMixerConfig,
    ) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let eq_config = EqConfig::builder(pools.clone()).build();
        let eq = if config.eq_bands() > 0 {
            let layout = EqLayout::new(
                &eq_config,
                &generate_log_spaced_bands(config.eq_bands()),
                config.sample_rate(),
            )?;
            StereoEq::with_layout(&eq_config, shape.sample_rate, Box::new(layout))
        } else {
            StereoEq::new(&eq_config, shape.sample_rate)
        };
        Ok(Self {
            gain: SmoothedParam::new(
                gain,
                Self::GAIN_SPAN,
                SmootherConfig::default(),
                shape.sample_rate,
            ),
            eq,
            priming: true,
        })
    }

    /// Whether this is the first range the deck renders; the output gain moves to its target.
    pub(crate) fn take_priming(&mut self) -> bool {
        let priming = std::mem::take(&mut self.priming);
        if priming {
            self.gain.reset_to_target();
        }
        priming
    }

    /// Pass the mixed frames of a stereo pair through the deck's gain and equaliser.
    pub(crate) fn finish(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.apply_gain(left, right);
        self.eq.process(left, right);
    }

    delegate::delegate! {
        to self.gain {
            /// Ramp the deck's output gain to `gain` from the next frame rendered.
            #[call(set_value)]
            pub(crate) fn set_gain(&mut self, gain: f32);
            /// Nothing sounded in a range: the output gain moves to its target at once.
            #[call(reset_to_target)]
            pub(crate) fn idle(&mut self);
        }
        to self.eq {
            #[call(read_gains)]
            pub(crate) fn read_eq(&self, gains: &mut [GainDb]) -> usize;
            /// Ramp `band` of the deck's equaliser to `gain` from the next frame rendered.
            #[call(set_gain)]
            pub(crate) fn set_eq_gain(&mut self, band: usize, gain: GainDb);
            /// Cross the deck's equaliser over to `layout`, answering the layout it displaced.
            #[call(take_layout)]
            pub(crate) fn take_eq_layout(&mut self, layout: Box<EqLayout>) -> Option<Box<EqLayout>>;
        }
    }

    pub(crate) fn update_sample_rate(&mut self, sample_rate: NonZeroU32) {
        self.gain.update_sample_rate(sample_rate);
        self.eq.update_sample_rate(sample_rate);
    }

    fn apply_gain(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.gain.has_settled() {
            let gain = self.gain.target_value();
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                *l *= gain;
                *r *= gain;
            }
            return;
        }
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let gain = self.gain.next_smoothed();
            *l *= gain;
            *r *= gain;
        }
        self.gain.settle();
    }
}
