#![forbid(unsafe_code)]

use std::{
    f64::consts::PI,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara_effects::{
    DeckEffect, FxParamChange,
    delay::{
        DelayParam, DelayTime, Feedback, FeedbackDelay, FeedbackDelayConfig, FeedbackHighPass,
    },
};
use kithara_test_utils::{bufpool::pools, kithara, kithara_platform::time::Duration};
use num_traits::ToPrimitive;

const RATES: [u32; 11] = [
    384_000, 768_000, 352_800, 705_600, 44_100, 48_000, 96_000, 192_000, 8_000, 326_461, 326_462,
];
const STEPS: u32 = 65_536;
const MODEL: f64 = 1e-6;

#[kithara::test(timeout(Duration::from_secs(60)))]
#[case::construction(false)]
#[case::retune(true)]
fn feedback_delay_high_pass_domain_has_no_holes(#[case] retune: bool) {
    let low = f64::from(f32::from(FeedbackHighPass::MIN));
    let high = f64::from(f32::from(FeedbackHighPass::MAX));
    for rate in RATES {
        let rate = NonZeroU32::new(rate).expect("host rate");
        for step in 0..=STEPS {
            let cutoff = (low * (high / low).powf(f64::from(step) / f64::from(STEPS)))
                .to_f32()
                .expect("cutoff fits f32");
            if f64::from(cutoff) >= f64::from(rate.get()) / 2.0 {
                continue;
            }
            let start = if retune {
                NonZeroU32::new(44_100).expect("initial rate")
            } else {
                rate
            };
            let high_pass = FeedbackHighPass::checked(cutoff).expect("cutoff in domain");
            let config = FeedbackDelayConfig::builder()
                .sample_rate(start)
                .capacity(NonZeroUsize::new(8).expect("history"))
                .time(DelayTime::MIN)
                .feedback(Feedback::checked(0.5).expect("stable feedback"))
                .high_pass(high_pass)
                .build();
            let mut delay = FeedbackDelay::new(config, &pools())
                .unwrap_or_else(|error| panic!("new at {start}, cutoff {cutoff}: {error:?}"));
            if retune {
                delay.update_sample_rate(rate);
            }
            assert!(
                delay.accepts(&FxParamChange::Delay(DelayParam::HighPass(high_pass))),
                "cutoff {cutoff} below Nyquist at {rate}, retune={retune}"
            );
            let input = [1.0, 0.0, 0.0];
            let mut wet = [[0.0; 3]; 2];
            let [left, right] = &mut wet;
            delay.process([&input, &input], [left, right]);
            // The first filtered repeat has the Butterworth high-pass's b0.
            let g = (PI * f64::from(cutoff) / f64::from(rate.get())).tan();
            let expected = 0.5 / (1.0 + 2.0_f64.sqrt() * g + g * g);
            for plane in wet {
                assert_eq!(plane[1], 1.0, "first echo remains unfiltered");
                assert!(
                    (f64::from(plane[2]) - expected).abs() <= MODEL,
                    "new-rate echo at {rate}, cutoff {cutoff}, retune={retune}: {} vs {expected}",
                    plane[2]
                );
            }
        }
    }
}
