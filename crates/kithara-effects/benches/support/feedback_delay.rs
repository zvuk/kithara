//! Shared pilot inputs for the feedback-delay benchmark artifacts: the same
//! prepared delay, case parameters and process step, so the timing, guard and
//! heap artifacts measure one workload with matching case boundaries.

use std::num::{NonZeroU32, NonZeroUsize};

use kithara_effects::{
    DeckEffect,
    delay::{DelayTime, Feedback, FeedbackDelay, FeedbackDelayConfig, FeedbackHighPass},
};
use kithara_test_utils::bufpool::pools;

/// The group every artifact declares its cases under.
pub(crate) const GROUP: &str = "feedback_comb";

/// The named cases every artifact registers: case id and delay frames.
pub(crate) const CASES: [(&str, f64); 3] = [
    ("integer_480", 480.0),
    ("integer_96k", 96_000.0),
    ("fractional_96k", 96_000.5),
];

/// One prepared high-pass feedback-delay case: the constructed delay plus the
/// stereo impulse input and the wet scratch buffers `process` writes into.
pub(crate) struct Prepared {
    delay: FeedbackDelay,
    input: [Vec<f32>; 2],
    wet: [Vec<f32>; 2],
}

/// Build one delay at `frames` and its impulse/scratch buffers outside the
/// measured work. The delay owns its pooled history, so the region may drop.
pub(crate) fn prepare(frames: f64) -> Prepared {
    let region = pools();
    let config = FeedbackDelayConfig::builder()
        .sample_rate(NonZeroU32::new(48_000).expect("rate"))
        .capacity(NonZeroUsize::new(192_000).expect("capacity"))
        .time(DelayTime::checked(frames).expect("delay"))
        .feedback(Feedback::checked(0.5).expect("gain"))
        .high_pass(FeedbackHighPass::checked(1000.0).expect("cutoff"))
        .build();
    let delay = FeedbackDelay::new(config, &region).expect("delay fits");
    let mut input = [vec![0.0_f32; 256], vec![0.0; 256]];
    input[0][0] = 1.0;
    input[1][0] = -0.5;
    let wet = input.clone();
    Prepared { delay, input, wet }
}

/// One stereo `process` pass over the prepared delay.
pub(crate) fn process_once(prepared: &mut Prepared) {
    let [input_l, input_r] = &prepared.input;
    let [wet_l, wet_r] = &mut prepared.wet;
    prepared.delay.process(
        std::hint::black_box([input_l.as_slice(), input_r.as_slice()]),
        std::hint::black_box([wet_l.as_mut_slice(), wet_r.as_mut_slice()]),
    );
}
