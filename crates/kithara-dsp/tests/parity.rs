#![forbid(unsafe_code)]

mod common;

use std::num::NonZeroUsize;

use common::oracle;
use fearless_simd::Level;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use kithara_dsp::Accelerate;
use kithara_dsp::{Backend, Portable};
use kithara_test_fixtures::signal::Wave;
use kithara_test_utils::kithara;

const SIZES: [usize; 15] = [0, 1, 3, 4, 5, 7, 8, 9, 15, 16, 17, 63, 64, 1023, 4096];
const OFFSETS: [usize; 2] = [0, 1];
const STRIDES: [usize; 5] = [1, 2, 3, 6, 9];
const RATE: u32 = 48_000;
const UNWRITTEN: f32 = -1.0;
const SPECIALS: [f32; 8] = [
    0.0,
    -0.0,
    f32::from_bits(1),
    f32::MIN_POSITIVE,
    f32::MAX,
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::NAN,
];
const SINE: Wave = Wave::Sine {
    hz: 440.0,
    peak: i16::MAX,
};

macro_rules! for_each_backend {
    ($check:ident) => {{
        $check("portable-native", &Portable::default());
        $check("portable-fallback", &Portable::new(Level::fallback()));
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        $check("accelerate", &Accelerate);
    }};
}

fn signal(len: usize, wave: Wave) -> Vec<f32> {
    SPECIALS
        .into_iter()
        .chain((0..).map(|frame| f32::from(wave.sample(frame, RATE)) / 32_768.0))
        .take(len)
        .collect()
}

fn bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|value| value.to_bits()).collect()
}

fn stride(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("strides are non-zero")
}

#[kithara::test]
fn interleave_pair_matches_the_oracle() {
    for_each_backend!(check_interleave_pair);
}

fn check_interleave_pair<B: Backend>(name: &str, backend: &B) {
    for size in SIZES {
        for offset in OFFSETS {
            let left = signal(size + offset, SINE);
            let right = signal(size + offset, Wave::Sawtooth);
            let mut expected = vec![UNWRITTEN; 2 * size + offset];
            let mut actual = expected.clone();
            let want =
                oracle::interleave_pair(&left[offset..], &right[offset..], &mut expected[offset..]);
            let got =
                backend.interleave_pair(&left[offset..], &right[offset..], &mut actual[offset..]);
            assert_eq!(got, want, "{name}: frames, size {size}, offset {offset}");
            assert_eq!(
                bits(&actual),
                bits(&expected),
                "{name}: size {size}, offset {offset}"
            );
        }
    }
}

#[kithara::test]
fn deinterleave_pair_matches_the_oracle() {
    for_each_backend!(check_deinterleave_pair);
}

fn check_deinterleave_pair<B: Backend>(name: &str, backend: &B) {
    for size in SIZES {
        for offset in OFFSETS {
            let input = signal(2 * size + offset, SINE);
            let mut want = (
                vec![UNWRITTEN; size + offset],
                vec![UNWRITTEN; size + offset],
            );
            let mut got = want.clone();
            let want_frames = oracle::deinterleave_pair(
                &input[offset..],
                &mut want.0[offset..],
                &mut want.1[offset..],
            );
            let got_frames = backend.deinterleave_pair(
                &input[offset..],
                &mut got.0[offset..],
                &mut got.1[offset..],
            );
            assert_eq!(
                got_frames, want_frames,
                "{name}: frames, size {size}, offset {offset}"
            );
            assert_eq!(
                bits(&got.0),
                bits(&want.0),
                "{name}: left, size {size}, offset {offset}"
            );
            assert_eq!(
                bits(&got.1),
                bits(&want.1),
                "{name}: right, size {size}, offset {offset}"
            );
        }
    }
}

#[kithara::test]
fn scatter_and_gather_match_the_oracle() {
    for_each_backend!(check_strided);
}

fn check_strided<B: Backend>(name: &str, backend: &B) {
    for size in SIZES {
        for step in STRIDES {
            for channel in 0..step {
                let plane = signal(size, Wave::Sawtooth);
                let mut want = vec![UNWRITTEN; size * step];
                let mut got = want.clone();
                let start = channel.min(want.len());
                let want_frames = oracle::scatter(&plane, &mut want[start..], stride(step));
                let got_frames = backend.scatter(&plane, &mut got[start..], stride(step));
                assert_eq!(
                    got_frames, want_frames,
                    "{name}: scatter frames, size {size}, stride {step}, channel {channel}"
                );
                assert_eq!(
                    bits(&got),
                    bits(&want),
                    "{name}: scatter, size {size}, stride {step}, channel {channel}"
                );

                let mut want_plane = vec![UNWRITTEN; size];
                let mut got_plane = want_plane.clone();
                let want_frames = oracle::gather(&want[start..], stride(step), &mut want_plane);
                let got_frames = backend.gather(&want[start..], stride(step), &mut got_plane);
                assert_eq!(
                    got_frames, want_frames,
                    "{name}: gather frames, size {size}, stride {step}, channel {channel}"
                );
                assert_eq!(
                    bits(&got_plane),
                    bits(&want_plane),
                    "{name}: gather, size {size}, stride {step}, channel {channel}"
                );
            }
        }
    }
}

#[kithara::test]
fn a_short_side_bounds_every_layout_kernel() {
    for_each_backend!(check_common_prefix);
}

fn check_common_prefix<B: Backend>(name: &str, backend: &B) {
    let mut output = [UNWRITTEN; 7];
    assert_eq!(
        backend.interleave_pair(&[1.0, 2.0, 3.0, 4.0], &[-1.0, -2.0, -3.0], &mut output),
        3,
        "{name}"
    );
    assert_eq!(
        bits(&output),
        bits(&[1.0, -1.0, 2.0, -2.0, 3.0, -3.0, UNWRITTEN]),
        "{name}"
    );

    let (mut left, mut right) = ([UNWRITTEN; 4], [UNWRITTEN; 4]);
    assert_eq!(
        backend.deinterleave_pair(&[1.0, -1.0, 2.0, -2.0, 3.0], &mut left, &mut right),
        2,
        "{name}"
    );
    assert_eq!(
        bits(&left),
        bits(&[1.0, 2.0, UNWRITTEN, UNWRITTEN]),
        "{name}"
    );
    assert_eq!(
        bits(&right),
        bits(&[-1.0, -2.0, UNWRITTEN, UNWRITTEN]),
        "{name}"
    );

    let mut output = [UNWRITTEN; 7];
    assert_eq!(
        backend.scatter(&[1.0, 2.0, 3.0, 4.0], &mut output, stride(3)),
        3,
        "{name}"
    );
    assert_eq!(
        bits(&output),
        bits(&[1.0, UNWRITTEN, UNWRITTEN, 2.0, UNWRITTEN, UNWRITTEN, 3.0]),
        "{name}"
    );

    let mut plane = [UNWRITTEN; 4];
    assert_eq!(
        backend.gather(&[1.0, 0.0, 0.0, 2.0, 0.0, 0.0, 3.0], stride(3), &mut plane),
        3,
        "{name}"
    );
    assert_eq!(bits(&plane), bits(&[1.0, 2.0, 3.0, UNWRITTEN]), "{name}");
}
