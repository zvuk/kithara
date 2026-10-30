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
            let frames = left.len().min(right.len());
            kithara_dsp::scale_in_place(&mut left[..frames], gain);
            kithara_dsp::scale_in_place(&mut right[..frames], gain);
            return;
        }
        let frames = left.len().min(right.len());
        let mut gains = [0.0; crate::consts::FADE_CHUNK];
        for (left, right) in left[..frames]
            .chunks_mut(crate::consts::FADE_CHUNK)
            .zip(right[..frames].chunks_mut(crate::consts::FADE_CHUNK))
        {
            let gains = &mut gains[..left.len()];
            for gain in gains.iter_mut() {
                *gain = self.gain.next_smoothed();
            }
            kithara_dsp::multiply_in_place(left, gains);
            kithara_dsp::multiply_in_place(right, gains);
        }
        self.gain.settle();
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn gain_matches_scalar_pcm_and_terminal_state() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        let shape = StreamShape::new(NonZeroU32::new(1024).expect("block"), rate);
        let mut pass = RenderPass::new(
            &crate::test_pools::pools(),
            shape,
            1.0,
            super::super::DeckMixerConfig::builder().eq_bands(0).build(),
        )
        .expect("render pass");
        for (target, settled) in [
            (1.0, false),
            (0.0, false),
            (0.5, false),
            (0.999_99, false),
            (0.5, true),
            (0.0, true),
        ] {
            for length in [0, 1, 255, 256, 257, 513] {
                for (left_extra, right_extra) in [(0, 0), (3, 0), (0, 3)] {
                    pass.gain = SmoothedParam::new(1.0, 1.0, SmootherConfig::default(), rate);
                    pass.set_gain(target);
                    if settled {
                        pass.gain.reset_to_target();
                        assert!(pass.gain.has_settled());
                    }
                    let mut expected_gain = pass.gain;
                    if target == 0.999_99 && length > crate::consts::FADE_CHUNK {
                        let mut premature = expected_gain;
                        premature.process_into_buffer(&mut [0.0; crate::consts::FADE_CHUNK]);
                        assert!(
                            premature.has_settled(),
                            "the first scratch chunk could settle early"
                        );
                    }
                    let mut left: Vec<_> = [0.25, -0.75, 0.0, -0.0]
                        .into_iter()
                        .cycle()
                        .take(length + left_extra)
                        .collect();
                    let mut right = vec![-0.5; length + right_extra];
                    let (mut expected_left, mut expected_right) = (left.clone(), right.clone());
                    let moving = !expected_gain.has_settled();
                    for (l, r) in expected_left.iter_mut().zip(&mut expected_right) {
                        let gain = if moving {
                            expected_gain.next_smoothed()
                        } else {
                            target
                        };
                        *l *= gain;
                        *r *= gain;
                    }
                    if moving {
                        expected_gain.settle();
                    }
                    pass.apply_gain(&mut left, &mut right);
                    assert_eq!(
                        left.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                        expected_left
                            .iter()
                            .map(|x| x.to_bits())
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(
                        right.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                        expected_right
                            .iter()
                            .map(|x| x.to_bits())
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(pass.gain, expected_gain, "target {target}, length {length}");
                }
            }
        }
    }
}
