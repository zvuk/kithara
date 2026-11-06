use fearless_simd::{Level, Simd, dispatch};

pub(crate) fn attack_release(release: f32, gain: &mut f32, targets: &mut [f32]) -> usize {
    dispatch!(Level::new(), simd => attack_release_kernel(simd, release, gain, targets))
}

#[inline(always)]
fn attack_release_kernel<S: Simd>(
    _simd: S,
    release: f32,
    gain: &mut f32,
    targets: &mut [f32],
) -> usize {
    for target in targets.iter_mut() {
        *gain = (1.0 - *gain).mul_add(-release, 1.0);
        if *target < *gain {
            *gain = *target;
        }
        *target = *gain;
    }
    targets.len()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn native_and_fallback_keep_the_same_recursion() {
        let mut native_gain = 1.0;
        let mut fallback_gain = 1.0;
        for count in [0, 1, 7, 255, 256, 257, 600] {
            let mut native: Vec<f32> = [1.0, 0.25, 0.8, 1.0, 0.0, 0.99]
                .into_iter()
                .cycle()
                .take(count)
                .collect();
            let mut fallback = native.clone();
            assert_eq!(
                attack_release(0.999_583_4, &mut native_gain, &mut native),
                count
            );
            assert_eq!(
                dispatch!(Level::fallback(), simd => attack_release_kernel(simd, 0.999_583_4, &mut fallback_gain, &mut fallback)),
                count
            );
            assert_eq!(
                native
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>(),
                fallback
                    .iter()
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>()
            );
            assert_eq!(native_gain.to_bits(), fallback_gain.to_bits());
        }
    }
}
