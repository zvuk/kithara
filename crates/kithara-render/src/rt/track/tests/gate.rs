use std::num::NonZeroU32;

use kithara_dsp::param::SmootherConfig;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use crate::rt::track::gate::TrackGate;

#[kithara::test]
fn the_gate_complements_the_slot_tail_across_render_ranges() {
    let mut gate = TrackGate::new(
        false,
        SmootherConfig {
            smooth_seconds: 0.004,
            ..Default::default()
        },
        NonZeroU32::new(1_000).expect("rate"),
    );
    gate.steer(true);
    let mut left = [1.0; 4];
    let mut right = left;
    gate.apply(&mut [&mut left, &mut right], 0..2);
    gate.apply(&mut [&mut left, &mut right], 2..4);
    assert_eq!(left, [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0]);
    assert_eq!(right, left);
    for (frame, gain) in left.into_iter().enumerate() {
        let tail = 1.0 - frame.to_f32().expect("frame") / 3.0;
        assert_eq!(tail + gain, 1.0);
    }
}
