use num_traits::cast::{AsPrimitive, ToPrimitive};

use super::frames::RATE;

const DECAY: f32 = 400.0;
const BURST_SECONDS: f32 = 0.01;

pub(crate) fn silence(seconds: f32) -> Vec<f32> {
    vec![0.0; samples(seconds)]
}

fn samples(seconds: f32) -> usize {
    (seconds * RATE).as_()
}

pub(crate) fn track(seconds: f32, period_seconds: f32) -> Vec<f32> {
    let mut pcm = silence(seconds);
    let step = samples(period_seconds);
    let burst = samples(BURST_SECONDS);
    for at in (0..pcm.len()).step_by(step.max(1)) {
        for (n, sample) in pcm[at..].iter_mut().take(burst).enumerate() {
            let t = n.to_f32().unwrap_or(0.0) / RATE;
            *sample = (-DECAY * t).exp();
        }
    }
    pcm
}

pub(crate) fn positions(seconds: f32, period_seconds: f32) -> Vec<f32> {
    let mut at = 0.0;
    let mut out = Vec::new();
    while at < seconds {
        out.push(at);
        at += period_seconds;
    }
    out
}
