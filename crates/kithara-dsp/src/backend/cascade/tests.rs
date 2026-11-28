use std::num::NonZeroUsize;

use fearless_simd::{Level, dispatch};
use kithara_test_fixtures::portable::harmonics;
use kithara_test_utils::kithara;

#[cfg(any(target_os = "macos", target_os = "ios"))]
use super::super::accelerate;
use super::super::cascade;
use crate::filter::FilterError;

const TWO: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);

// Distinct stable sections make broadcasting a lane's coefficients observable.
const SECTIONS: [[f32; 5]; 2] = [
    [0.5, 0.25, 0.125, -0.25, 0.125],
    [0.25, -0.125, 0.0625, -0.5, 0.125],
];

macro_rules! check_planes {
    ($backend:ty, $process:expr) => {{
        let input = harmonics();
        for channels in [1, 2, 3, 8] {
            let count = NonZeroUsize::new(channels).expect("nonzero channels");
            let mut together = <$backend>::new(count, TWO).expect("cascade builds");
            let mut twin = <$backend>::new(count, TWO).expect("cascade builds");
            let mut actual: Vec<Vec<f32>> = input
                .get(..channels)
                .expect("fixture contains every channel")
                .iter()
                .map(|plane| plane.to_vec())
                .collect();
            let mut expected = actual.clone();
            for (channel, plane) in expected.iter_mut().enumerate() {
                let mut single = <$backend>::new(NonZeroUsize::MIN, TWO).expect("one plane");
                for (section, coefficients) in SECTIONS.iter().copied().enumerate() {
                    let mut coefficients = coefficients;
                    coefficients[0] += f32::from(u8::try_from(channel).expect("small lane")) / 32.0;
                    together
                        .set_section(section, channel..channel.saturating_add(1), coefficients)
                        .expect("lane exists");
                    single
                        .set_section(section, 0..1, coefficients)
                        .expect("section exists");
                }
                let frames = plane.len();
                $process(&mut single, std::slice::from_mut(plane), 0..frames);
            }
            let frames = actual.first().expect("nonzero channels").len();
            $process(&mut together, &mut actual, 0..frames / 2);
            for channel in 0..channels {
                for (section, coefficients) in SECTIONS.iter().copied().enumerate() {
                    let mut coefficients = coefficients;
                    coefficients[0] += f32::from(u8::try_from(channel).expect("small lane")) / 32.0;
                    twin.set_section(section, channel..channel.saturating_add(1), coefficients)
                        .expect("lane exists");
                }
            }
            twin.copy_state_from(&together).expect("same shape");
            $process(&mut twin, &mut actual, frames / 2..frames);
            for (sample, (actual, expected)) in actual
                .iter()
                .flatten()
                .zip(expected.iter().flatten())
                .enumerate()
            {
                assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "{}: {channels} planes, sample {sample}",
                    stringify!($backend)
                );
            }
            assert_eq!(
                together.set_section(0, channels..channels.saturating_add(1), SECTIONS[0]),
                Err(FilterError::Shape)
            );
            assert_eq!(
                together.set_section(2, 0..1, SECTIONS[0]),
                Err(FilterError::Shape)
            );
            assert_eq!(
                together.set_section(0, 0..0, SECTIONS[0]),
                Err(FilterError::Shape)
            );
        }
    }};
}

#[kithara::test]
fn cascade_planes_equal_individual_filters_bit_for_bit() {
    check_planes!(
        cascade::Cascade,
        |filter: &mut cascade::Cascade, planes: &mut [Vec<f32>], range| {
            filter.process(planes, range).expect("matching planes");
        }
    );
    check_planes!(
        cascade::Cascade,
        |filter: &mut cascade::Cascade, planes: &mut [Vec<f32>], range| {
            dispatch!(Level::fallback(), simd => cascade::cascade_kernel(simd, filter, planes, range));
        }
    );
}

#[kithara::test(platform = apple)]
fn accelerate_cascade_planes_equal_individual_filters_bit_for_bit() {
    check_planes!(
        accelerate::Cascade,
        |filter: &mut accelerate::Cascade, planes: &mut [Vec<f32>], range| {
            filter.process(planes, range).expect("matching planes");
        }
    );
}
