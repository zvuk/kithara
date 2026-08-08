use super::super::Scroll;

pub(crate) const PIXELS_PER_STEP: f32 = 20.0;

/// Scroll deltas arrive content-directed (macOS natural scrolling): an
/// upward gesture is negative, so the value axis is the negated delta. A line
/// delta is one step, while trackpad pixels accumulate to the step threshold.
pub(crate) fn steps(accum: &mut f32, scroll: Scroll) -> f32 {
    match scroll {
        Scroll::Lines { y, .. } => {
            if y == 0.0 {
                return 0.0;
            }
            -y.signum()
        }
        Scroll::Pixels { y, .. } => {
            *accum -= y;
            let steps = (*accum / PIXELS_PER_STEP).trunc();
            *accum -= steps * PIXELS_PER_STEP;
            steps
        }
    }
}
