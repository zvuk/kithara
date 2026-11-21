use fearless_simd::{Level, dispatch};
use kithara_test_utils::kithara;

use super::{kernel::low_pass_kernel, low_pass};

fn parity<const CHANNELS: usize>() {
    let mut native_state = [0.0; CHANNELS];
    let mut fallback_state = native_state;
    for frames in [0, 1, 2, 3, 4, 5, 7, 15, 255, 256, 257, 600] {
        let mut native: [Vec<f32>; CHANNELS] = std::array::from_fn(|channel| {
            [0.0, -0.0, 0.75, -0.25, 1e-30, 0.125]
                .into_iter()
                .cycle()
                .skip(channel)
                .take(frames)
                .collect()
        });
        let mut fallback = native.clone();
        let mut native_planes = native.each_mut().map(Vec::as_mut_slice);
        let mut fallback_planes = fallback.each_mut().map(Vec::as_mut_slice);
        assert_eq!(
            low_pass(0.875, &mut native_state, &mut native_planes),
            frames
        );
        assert_eq!(
            dispatch!(Level::fallback(), simd => low_pass_kernel(simd,
            0.875, &mut fallback_state, &mut fallback_planes)),
            frames
        );
        for (native, fallback) in native.iter().flatten().zip(fallback.iter().flatten()) {
            assert_eq!(native.to_bits(), fallback.to_bits());
        }
        assert_eq!(
            native_state.map(f32::to_bits),
            fallback_state.map(f32::to_bits)
        );
    }
}

#[kithara::test]
fn native_and_fallback_keep_the_same_low_pass_recursion() {
    parity::<1>();
    parity::<2>();
    parity::<3>();
    parity::<4>();
    parity::<5>();
    parity::<7>();
    parity::<8>();
    parity::<9>();
}
