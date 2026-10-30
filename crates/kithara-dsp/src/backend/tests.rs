use std::{num::NonZeroUsize, ops::Range};

use fearless_simd::{Level, dispatch};
use kithara_test_fixtures::{
    portable::{harmonics, sawtooth, sine_440},
    signal::Wave,
};
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

#[cfg(any(target_os = "macos", target_os = "ios"))]
use super::accelerate;
use super::{cascade, fade, platform, portable, simd, strided};
use crate::{
    fade::FadeCurve,
    filter::{Coefficients, Hertz, Type},
    interp::{InterpError, Interpolation},
};

const SIZES: [usize; 15] = [0, 1, 3, 4, 5, 7, 8, 9, 15, 16, 17, 63, 64, 1023, 4096];
const OFFSETS: [usize; 2] = [0, 1];
const STRIDES: [usize; 5] = [1, 2, 3, 6, 9];
const RATE: u32 = 48_000;
const UNWRITTEN: f32 = -1.0;
/// A signaling `NaN`: a plain copy keeps its quiet bit clear.
const SIGNALING_NAN: u32 = 0x7F80_0001;
const SPECIALS: [f32; 9] = [
    0.0,
    -0.0,
    f32::from_bits(1),
    f32::MIN_POSITIVE,
    f32::MAX,
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::NAN,
    f32::from_bits(SIGNALING_NAN),
];

type DeinterleavePair = fn(&[f32], &mut [f32], &mut [f32]) -> usize;
type DownmixPair = fn(&[f32], &mut [f32]) -> usize;
type InterleavePair = fn(&[f32], &[f32], &mut [f32]) -> usize;

/// One backend's pair kernels, so every check runs the same matrix on each.
struct Pairs {
    name: &'static str,
    deinterleave_pair: DeinterleavePair,
    downmix_pair: DownmixPair,
    interleave_pair: InterleavePair,
}

fn pairs() -> Vec<Pairs> {
    Vec::from([
        Pairs {
            name: "portable-native",
            deinterleave_pair: portable::deinterleave_pair,
            downmix_pair: portable::downmix_pair,
            interleave_pair: portable::interleave_pair,
        },
        Pairs {
            name: "portable-fallback",
            deinterleave_pair: |input, left, right| dispatch!(Level::fallback(), simd => portable::deinterleave_pair_kernel(simd, input, left, right)),
            downmix_pair: |input, mono| dispatch!(Level::fallback(), simd => portable::downmix_pair_kernel(simd, input, mono)),
            interleave_pair: |left, right, output| dispatch!(Level::fallback(), simd => portable::interleave_pair_kernel(simd, left, right, output)),
        },
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        Pairs {
            name: "accelerate",
            deinterleave_pair: accelerate::deinterleave_pair,
            downmix_pair: accelerate::downmix_pair,
            interleave_pair: accelerate::interleave_pair,
        },
    ])
}

fn signal(len: usize, prepared: &[f32]) -> Vec<f32> {
    let mut samples = Vec::with_capacity(len);
    samples.extend_from_slice(
        SPECIALS
            .get(..len.min(SPECIALS.len()))
            .expect("special input extent"),
    );
    samples.extend_from_slice(
        prepared
            .get(..len.saturating_sub(SPECIALS.len()))
            .expect("prepared input extent"),
    );
    samples
}

fn bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|value| value.to_bits()).collect()
}

fn stride(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("strides are non-zero")
}

#[kithara::test]
fn interleave_pair_matches_the_oracle() {
    for layout in pairs() {
        let name = layout.name;
        for size in SIZES {
            for offset in OFFSETS {
                let left = signal(size.saturating_add(offset), sine_440());
                let right = signal(size.saturating_add(offset), sawtooth());
                let mut expected = vec![UNWRITTEN; size.saturating_mul(2).saturating_add(offset)];
                let mut actual = expected.clone();
                let ((_, left), (_, right)) = (left.split_at(offset), right.split_at(offset));
                let want = oracle::interleave_pair(left, right, expected.split_at_mut(offset).1);
                let got = (layout.interleave_pair)(left, right, actual.split_at_mut(offset).1);
                assert_eq!(got, want, "{name}: frames, size {size}, offset {offset}");
                assert_eq!(
                    bits(&actual),
                    bits(&expected),
                    "{name}: size {size}, offset {offset}"
                );
            }
        }
    }
}

#[kithara::test]
fn deinterleave_pair_matches_the_oracle() {
    for layout in pairs() {
        let name = layout.name;
        for size in SIZES {
            for offset in OFFSETS {
                let input = signal(size.saturating_mul(2).saturating_add(offset), sine_440());
                let input = input.split_at(offset).1;
                let plane = vec![UNWRITTEN; size.saturating_add(offset)];
                let (mut want, mut got) = ((plane.clone(), plane.clone()), (plane.clone(), plane));
                let want_frames = oracle::deinterleave_pair(
                    input,
                    want.0.split_at_mut(offset).1,
                    want.1.split_at_mut(offset).1,
                );
                let got_frames = (layout.deinterleave_pair)(
                    input,
                    got.0.split_at_mut(offset).1,
                    got.1.split_at_mut(offset).1,
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
}

/// `(l + r) · 0.5` bit for bit, the formula every analysis path used
/// before `downmix`; a `NaN` payload is the backend's to choose.
#[kithara::test]
fn downmix_pair_matches_the_scalar_mean() {
    for layout in pairs() {
        let name = layout.name;
        for size in SIZES {
            for offset in OFFSETS {
                let input = signal(
                    size.saturating_mul(2)
                        .saturating_add(offset)
                        .saturating_add(1),
                    sine_440(),
                );
                let input = input.split_at(offset).1;
                let mut mono = vec![UNWRITTEN; size.saturating_add(2)];
                assert_eq!(
                    (layout.downmix_pair)(input, &mut mono),
                    size,
                    "{name}: frames, size {size}, offset {offset}"
                );
                for (index, (got, [left, right])) in
                    mono.iter().zip(input.as_chunks::<2>().0).enumerate()
                {
                    assert!(
                        same(*got, (left + right) * 0.5),
                        "{name}: mean of {left} and {right} at {index}, size {size}, offset {offset}"
                    );
                }
                assert!(
                    mono.iter()
                        .skip(size)
                        .all(|value| value.to_bits() == UNWRITTEN.to_bits()),
                    "{name}: wrote past {size}, offset {offset}"
                );
            }
        }
    }
}

#[kithara::test]
fn scatter_and_gather_match_the_oracle() {
    for size in SIZES {
        for step in STRIDES {
            for channel in 0..step {
                let plane = signal(size, sawtooth());
                let mut want = vec![UNWRITTEN; size.saturating_mul(step)];
                let mut got = want.clone();
                let start = channel.min(want.len());
                let want_frames = oracle::scatter(&plane, want.split_at_mut(start).1, stride(step));
                let got_frames = strided::scatter(&plane, got.split_at_mut(start).1, stride(step));
                assert_eq!(
                    got_frames, want_frames,
                    "scatter frames, size {size}, stride {step}, channel {channel}"
                );
                assert_eq!(
                    bits(&got),
                    bits(&want),
                    "scatter, size {size}, stride {step}, channel {channel}"
                );

                let interleaved = want.split_at(start).1;
                let mut want_plane = vec![UNWRITTEN; size];
                let mut got_plane = want_plane.clone();
                let want_frames = oracle::gather(interleaved, stride(step), &mut want_plane);
                let got_frames = strided::gather(interleaved, stride(step), &mut got_plane);
                assert_eq!(
                    got_frames, want_frames,
                    "gather frames, size {size}, stride {step}, channel {channel}"
                );
                assert_eq!(
                    bits(&got_plane),
                    bits(&want_plane),
                    "gather, size {size}, stride {step}, channel {channel}"
                );
            }
        }
    }
}

#[kithara::test]
fn sanitize_matches_the_oracle_at_every_level() {
    let fallback = |samples: &mut [f32]| {
        dispatch!(Level::fallback(), simd => simd::sanitize_kernel(simd, samples));
    };
    for (name, sanitize) in [
        ("native", simd::sanitize as fn(&mut [f32])),
        ("fallback", fallback),
    ] {
        for size in SIZES {
            for offset in OFFSETS {
                let mut want = signal(size.saturating_add(offset), sine_440());
                let mut got = want.clone();
                oracle::sanitize(want.split_at_mut(offset).1);
                sanitize(got.split_at_mut(offset).1);
                assert_eq!(
                    bits(&got),
                    bits(&want),
                    "{name}: size {size}, offset {offset}"
                );
            }
        }
    }
}

#[kithara::test]
fn a_short_side_bounds_every_pair_kernel() {
    for layout in pairs() {
        let name = layout.name;
        let mut output = [UNWRITTEN; 7];
        assert_eq!(
            (layout.interleave_pair)(&[1.0, 2.0, 3.0, 4.0], &[-1.0, -2.0, -3.0], &mut output),
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
            (layout.deinterleave_pair)(&[1.0, -1.0, 2.0, -2.0, 3.0], &mut left, &mut right),
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

        let mut mono = [UNWRITTEN; 3];
        assert_eq!(
            (layout.downmix_pair)(&[1.0, 3.0, -2.0, 2.0, 5.0], &mut mono),
            2,
            "{name}"
        );
        assert_eq!(bits(&mono), bits(&[2.0, 0.0, UNWRITTEN]), "{name}");
        let mut mono = [UNWRITTEN; 1];
        assert_eq!(
            (layout.downmix_pair)(&[1.0, 3.0, 5.0, 7.0], &mut mono),
            1,
            "{name}"
        );
        assert_eq!(bits(&mono), bits(&[2.0]), "{name}");
    }
}

#[kithara::test]
fn a_short_side_bounds_the_strided_copies() {
    let mut output = [UNWRITTEN; 7];
    assert_eq!(
        strided::scatter(&[1.0, 2.0, 3.0, 4.0], &mut output, stride(3)),
        3
    );
    assert_eq!(
        bits(&output),
        bits(&[1.0, UNWRITTEN, UNWRITTEN, 2.0, UNWRITTEN, UNWRITTEN, 3.0])
    );

    let mut plane = [UNWRITTEN; 4];
    assert_eq!(
        strided::gather(&[1.0, 0.0, 0.0, 2.0, 0.0, 0.0, 3.0], stride(3), &mut plane),
        3
    );
    assert_eq!(bits(&plane), bits(&[1.0, 2.0, 3.0, UNWRITTEN]));
}

/// Scalar reference for the kernels: index arithmetic instead of lanes.
mod oracle {
    use std::num::NonZeroUsize;

    pub(super) fn interleave_pair(left: &[f32], right: &[f32], output: &mut [f32]) -> usize {
        let frames = left.len().min(right.len()).min(output.len() / 2);
        for ((pair, l), r) in output
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(left)
            .zip(right)
            .take(frames)
        {
            pair.copy_from_slice(&[*l, *r]);
        }
        frames
    }

    pub(super) fn deinterleave_pair(input: &[f32], left: &mut [f32], right: &mut [f32]) -> usize {
        let frames = (input.len() / 2).min(left.len()).min(right.len());
        for ((pair, l), r) in input
            .as_chunks::<2>()
            .0
            .iter()
            .zip(left.iter_mut())
            .zip(right.iter_mut())
            .take(frames)
        {
            [*l, *r] = *pair;
        }
        frames
    }

    pub(super) fn scatter(plane: &[f32], output: &mut [f32], stride: NonZeroUsize) -> usize {
        let frames = plane.len().min(output.len().div_ceil(stride.get()));
        for (frame, sample) in plane.iter().take(frames).enumerate() {
            if let Some(slot) = frame
                .checked_mul(stride.get())
                .and_then(|slot| output.get_mut(slot))
            {
                *slot = *sample;
            }
        }
        frames
    }

    pub(super) fn gather(input: &[f32], stride: NonZeroUsize, plane: &mut [f32]) -> usize {
        let frames = plane.len().min(input.len().div_ceil(stride.get()));
        for (frame, slot) in plane.iter_mut().take(frames).enumerate() {
            if let Some(sample) = frame
                .checked_mul(stride.get())
                .and_then(|slot| input.get(slot))
            {
                *slot = *sample;
            }
        }
        frames
    }

    pub(super) fn sanitize(samples: &mut [f32]) {
        for sample in samples {
            if !sample.is_finite() || sample.abs() < f32::MIN_POSITIVE {
                *sample = 0.0;
            }
        }
    }
}

/// Largest error of a cascade against the `f64` oracle, relative to the
/// oracle's peak: −100 dB.
const PARITY: f64 = 1.0e-5;
const CUTOFFS: [[f64; 4]; 2] = [
    [1_000.0, 2_000.0, 4_000.0, 8_000.0],
    [3_000.0, 5_000.0, 7_000.0, 9_000.0],
];
/// Peak inputs: specials without `NaN` and subnormals, which `max` orders differently.
const PEAK_SPECIALS: [f32; 6] = [
    0.0,
    -0.0,
    f32::MIN_POSITIVE,
    f32::MAX,
    f32::INFINITY,
    f32::NEG_INFINITY,
];

/// Section coefficients per half of the stream: the second half retunes.
fn halves(sections: usize) -> [Vec<[f32; 5]>; 2] {
    CUTOFFS.map(|cutoffs| {
        cutoffs
            .iter()
            .take(sections)
            .map(|cutoff| {
                let Coefficients { a1, a2, b0, b1, b2 } = Coefficients::from_params(
                    Type::LowPass,
                    Hertz::from_hz(f64::from(RATE)).expect("positive rate"),
                    Hertz::from_hz(*cutoff).expect("positive cutoff"),
                    std::f64::consts::FRAC_1_SQRT_2,
                )
                .expect("valid low-pass");
                [b0, b1, b2, a1, a2].map(|value| value.to_f32().expect("coefficient fits f32"))
            })
            .collect()
    })
}

fn tones(channels: usize) -> Vec<Vec<f32>> {
    harmonics()
        .get(..channels)
        .expect("prepared channel extent")
        .iter()
        .map(|plane| plane.to_vec())
        .collect()
}

/// Direct form I in `f64` over the same `f32` coefficients.
fn oracle(input: &[Vec<f32>], halves: &[Vec<[f32; 5]>; 2]) -> Vec<Vec<f64>> {
    input
        .iter()
        .map(|plane| {
            let [first, second] = halves;
            let mut state = vec![[0.0_f64; 4]; first.len()];
            let half = plane.len().wrapping_div(2);
            plane
                .iter()
                .enumerate()
                .map(|(frame, sample)| {
                    let set = if frame < half { first } else { second };
                    set.iter().zip(state.iter_mut()).fold(
                        f64::from(*sample),
                        |x, (section, delay)| {
                            let [b0, b1, b2, a1, a2] = section.map(f64::from);
                            let [x1, x2, y1, y2] = *delay;
                            let y = b0.mul_add(
                                x,
                                b1.mul_add(x1, b2.mul_add(x2, (-a1).mul_add(y1, -a2 * y2))),
                            );
                            *delay = [x, x1, y, y1];
                            y
                        },
                    )
                })
                .collect()
        })
        .collect()
}

type RunCascade = fn(&mut [Vec<f32>], &[Vec<[f32; 5]>; 2]);

/// Drives a cascade through its whole interface the way glide does: a stale
/// state cleared by `reset`, then the second half on a twin that took over
/// the state and retuned.
macro_rules! handover_run {
    ($cascade:ty) => {
        |planes: &mut [Vec<f32>], halves: &[Vec<[f32; 5]>; 2]| {
            let channels = NonZeroUsize::new(planes.len()).expect("channels");
            let sections = NonZeroUsize::new(halves[0].len()).expect("sections");
            let mut cascade = <$cascade>::new(channels, sections).expect("cascade");
            let mut twin = <$cascade>::new(channels, sections).expect("cascade");
            let frames = planes.first().map_or(0, Vec::len);
            let half = frames.wrapping_div(2);
            for (section, coefficients) in halves[0].iter().enumerate() {
                cascade
                    .set_section(section, *coefficients)
                    .expect("section");
            }
            cascade
                .process(&mut planes.to_vec(), 0..frames)
                .expect("shape matches");
            cascade.reset();
            cascade.process(planes, 0..half).expect("shape matches");
            twin.copy_state_from(&cascade).expect("same shape");
            for (section, coefficients) in halves[1].iter().enumerate() {
                twin.set_section(section, *coefficients).expect("section");
            }
            twin.process(planes, half..frames).expect("shape matches");
        }
    };
}

/// Retunes the live state in place through the kernel at `Level::fallback()`.
fn portable_fallback_run(planes: &mut [Vec<f32>], halves: &[Vec<[f32; 5]>; 2]) {
    let channels = NonZeroUsize::new(planes.len()).expect("channels");
    let sections = NonZeroUsize::new(halves[0].len()).expect("sections");
    let mut cascade = cascade::Cascade::new(channels, sections).expect("cascade");
    let frames = planes.first().map_or(0, Vec::len);
    let half = frames.wrapping_div(2);
    for (range, set) in [(0..half, &halves[0]), (half..frames, &halves[1])] {
        for (section, coefficients) in set.iter().enumerate() {
            cascade
                .set_section(section, *coefficients)
                .expect("section");
        }
        dispatch!(Level::fallback(), simd => cascade::cascade_kernel(simd, &mut cascade, planes, range));
    }
}

#[kithara::test]
fn every_cascade_backend_tracks_the_f64_oracle_across_a_retune() {
    let runs: Vec<(&str, RunCascade)> = Vec::from([
        (
            "portable-native",
            handover_run!(cascade::Cascade) as RunCascade,
        ),
        ("portable-fallback", portable_fallback_run),
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        ("accelerate", handover_run!(accelerate::Cascade)),
    ]);
    for channels in [1_usize, 2, 8] {
        for sections in [1_usize, 4] {
            let halves = halves(sections);
            let input = tones(channels);
            let expected = oracle(&input, &halves);
            for (name, run) in &runs {
                let mut planes = input.clone();
                run(&mut planes, &halves);
                for (plane, reference) in planes.iter().zip(&expected) {
                    let peak = reference.iter().fold(0.0_f64, |peak, y| peak.max(y.abs()));
                    let worst = plane.iter().zip(reference).fold(0.0_f64, |worst, (y, r)| {
                        worst.max((f64::from(*y) - r).abs())
                    });
                    assert!(
                        worst <= PARITY * peak,
                        "{name}: {channels} ch × {sections} sections off by {worst} of {peak}"
                    );
                }
            }
        }
    }
}

type CascadeCall = fn(&mut cascade::Cascade, &mut [Vec<f32>], Range<usize>);

fn native_call(cascade: &mut cascade::Cascade, planes: &mut [Vec<f32>], range: Range<usize>) {
    cascade.process(planes, range).expect("shape matches");
}

fn fallback_call(cascade: &mut cascade::Cascade, planes: &mut [Vec<f32>], range: Range<usize>) {
    dispatch!(Level::fallback(), simd => cascade::cascade_kernel(simd, cascade, planes, range));
}

/// Channel 0 plays a tone while channel 1 rings out from one impulse through
/// glide's low-pass below unity (`0.9 · sr / 2`), eight frames per call.
#[kithara::test]
fn a_silent_channel_beside_a_playing_one_never_reaches_subnormals() {
    const CALL: usize = 8;
    const CALLS: usize = 1_024;
    let Coefficients { a1, a2, b0, b1, b2 } = Coefficients::from_params(
        Type::LowPass,
        Hertz::from_hz(f64::from(RATE)).expect("positive rate"),
        Hertz::from_hz(0.45 * f64::from(RATE)).expect("positive cutoff"),
        std::f64::consts::FRAC_1_SQRT_2,
    )
    .expect("valid low-pass");
    let section = [b0, b1, b2, a1, a2].map(|value| value.to_f32().expect("coefficient fits f32"));
    let runs: [(&str, CascadeCall); 2] = [
        ("portable-native", native_call),
        ("portable-fallback", fallback_call),
    ];
    for (name, call) in runs {
        let mut cascade = cascade::Cascade::new(stride(2), stride(1)).expect("cascade");
        cascade.set_section(0, section).expect("section");
        assert_eq!(sine_440().len(), CALL.saturating_mul(CALLS));
        let subnormal: usize = sine_440()
            .as_chunks::<CALL>()
            .0
            .iter()
            .enumerate()
            .map(|(block, tone)| {
                let tone = tone.to_vec();
                let mut ring = vec![0.0; CALL];
                if let Some(first) = ring.first_mut().filter(|_| block == 0) {
                    *first = 1.0;
                }
                let mut planes = [tone, ring];
                call(&mut cascade, &mut planes, 0..CALL);
                let [_, ring] = &planes;
                ring.iter().filter(|sample| sample.is_subnormal()).count()
            })
            .sum();
        assert_eq!(
            subnormal, 0,
            "{name}: the silent channel rang in subnormals"
        );
    }
}

fn fallback_peak(samples: &[f32]) -> f32 {
    dispatch!(Level::fallback(), simd => portable::peak_kernel(simd, samples))
}

#[kithara::test]
fn peak_matches_the_scalar_maximum_on_every_backend() {
    type Peak = fn(&[f32]) -> f32;
    let backends: Vec<(&str, Peak)> = Vec::from([
        ("portable-native", portable::peak as Peak),
        ("portable-fallback", fallback_peak),
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        ("accelerate", accelerate::peak),
    ]);
    for len in SIZES {
        let sine: Vec<f32> = signal(len.saturating_add(SPECIALS.len()), sine_440())
            .into_iter()
            .skip(SPECIALS.len())
            .collect();
        let with_specials: Vec<f32> = PEAK_SPECIALS
            .iter()
            .copied()
            .chain(sine.iter().copied())
            .take(len)
            .collect();
        for samples in [&sine, &with_specials] {
            let expected = samples
                .iter()
                .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
            for (name, peak) in &backends {
                assert_eq!(
                    peak(samples).to_bits(),
                    expected.to_bits(),
                    "{name} at {len}"
                );
            }
        }
    }
}

#[cfg(feature = "spectrum")]
type Multiply = fn(&[f32], &[f32], &mut [f32]) -> usize;
type SumSquares = fn(&[f32]) -> f32;
type WithScalars = fn(&mut [f32], f32, f32) -> usize;
type Sub = fn(&mut [f32], &[f32]) -> usize;
type ScaleAdd = fn(&[f32], f32, &mut [f32]) -> usize;
type MulAdd = fn(&[f32], &[f32], &mut [f32]) -> usize;
type Accumulate = fn(&[f32], &mut [f32]) -> usize;

/// One backend's element-wise kernels, so every check runs the same matrix
/// on each.
struct Vectors {
    name: &'static str,
    #[cfg(feature = "spectrum")]
    multiply: Multiply,
    sum_squares: SumSquares,
    affine: WithScalars,
    sub: Sub,
    scale_add: ScaleAdd,
    add: Accumulate,
}

fn vectors() -> Vec<Vectors> {
    Vec::from([
        Vectors {
            name: "portable-native",
            #[cfg(feature = "spectrum")]
            multiply: portable::multiply,
            sum_squares: portable::sum_squares,
            affine: portable::affine,
            sub: portable::sub,
            scale_add: portable::scale_add,
            add: portable::add,
        },
        Vectors {
            name: "portable-fallback",
            #[cfg(feature = "spectrum")]
            multiply: |a, b, output| dispatch!(Level::fallback(), simd => portable::multiply_kernel(simd, a, b, output)),
            sum_squares: |samples| dispatch!(Level::fallback(), simd => portable::sum_squares_kernel(simd, samples)),
            affine: |samples, scale, offset| dispatch!(Level::fallback(), simd => portable::affine_kernel(simd, samples, scale, offset)),
            sub: |x, y| dispatch!(Level::fallback(), simd => portable::sub_kernel(simd, x, y)),
            scale_add: |x, gain, acc| dispatch!(Level::fallback(), simd => portable::scale_add_kernel(simd, x, gain, acc)),
            add: |x, acc| dispatch!(Level::fallback(), simd => portable::add_kernel(simd, x, acc)),
        },
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        Vectors {
            name: "accelerate",
            #[cfg(feature = "spectrum")]
            multiply: accelerate::multiply,
            sum_squares: accelerate::sum_squares,
            affine: accelerate::affine,
            sub: accelerate::sub,
            scale_add: accelerate::scale_add,
            add: accelerate::add,
        },
    ])
}

/// The `ramp` kernel at the native and the fallback level: one kernel on
/// every platform.
fn ramps() -> [(&'static str, WithScalars); 2] {
    [
        ("native", simd::ramp),
        (
            "fallback",
            |output, start, step| dispatch!(Level::fallback(), simd => simd::ramp_kernel(simd, output, start, step)),
        ),
    ]
}

/// The `mul_add` kernel at the native and the fallback level: one kernel on
/// every platform.
fn mul_adds() -> [(&'static str, MulAdd); 2] {
    [
        ("native", simd::mul_add),
        (
            "fallback",
            |x, y, acc| dispatch!(Level::fallback(), simd => simd::mul_add_kernel(simd, x, y, acc)),
        ),
    ]
}

/// Equal bits, or both `NaN`: a `NaN` payload is the backend's to choose.
fn same(got: f32, want: f32) -> bool {
    got.to_bits() == want.to_bits() || (got.is_nan() && want.is_nan())
}

#[cfg(feature = "spectrum")]
#[kithara::test]
fn multiply_matches_the_scalar_product_on_every_backend() {
    for row in vectors() {
        let name = row.name;
        for size in SIZES {
            let a = signal(size, sine_440());
            let b = signal(size.saturating_add(1), sawtooth());
            let mut output = vec![UNWRITTEN; size.saturating_add(2)];
            assert_eq!(
                (row.multiply)(&a, &b, &mut output),
                size,
                "{name}: count at {size}"
            );
            for (index, ((got, x), y)) in output.iter().zip(&a).zip(&b).enumerate() {
                assert!(same(*got, x * y), "{name}: {x} · {y} at {index} of {size}");
            }
            assert!(
                output
                    .iter()
                    .skip(size)
                    .all(|value| value.to_bits() == UNWRITTEN.to_bits()),
                "{name}: wrote past {size}"
            );
        }
    }
}

/// `Σx²` within the reduction bound `N·ε·Σx²` of the `f64` sum (spec,
/// reduction level); a `NaN` or an infinity carries through.
#[kithara::test]
fn sum_squares_tracks_the_f64_sum_on_every_backend() {
    for row in vectors() {
        let name = row.name;
        for size in SIZES {
            let samples: Vec<f32> = signal(size.saturating_add(SPECIALS.len()), sine_440())
                .into_iter()
                .skip(SPECIALS.len())
                .collect();
            let want: f64 = samples.iter().map(|x| f64::from(*x) * f64::from(*x)).sum();
            let reach = size.to_f64().unwrap_or(f64::NAN) * f64::from(f32::EPSILON) * want;
            let got = (row.sum_squares)(&samples);
            assert!(
                (f64::from(got) - want).abs() <= reach,
                "{name}: {got}, f64 {want} at {size}"
            );
        }
        assert_eq!(
            (row.sum_squares)(&[]).to_bits(),
            0.0_f32.to_bits(),
            "{name}: an empty slice"
        );
        assert!(
            (row.sum_squares)(&[1.0, f32::NAN, 2.0]).is_nan(),
            "{name}: a NaN carries through"
        );
        assert_eq!(
            (row.sum_squares)(&[1.0, f32::NEG_INFINITY]).to_bits(),
            f32::INFINITY.to_bits(),
            "{name}: an infinity carries through"
        );
    }
}

/// Largest interpolation error against the `f64` oracle, relative to the
/// window peak: four `f32` epsilons.
const INTERP_PARITY: f64 = 4.768_371_582_031_25e-7;
const INTERP_WINDOW: u16 = 257;
const INTERP_POSITIONS: u16 = 1_000;
const INTERP_METHODS: [Interpolation; 4] = [
    Interpolation::Linear,
    Interpolation::Quadratic,
    Interpolation::Hermite,
    Interpolation::Watte,
];

type Interp = fn(Interpolation, &[f32], &[f32], &mut [f32]) -> Result<usize, InterpError>;

/// The textbook formulas in `f64` over the same `f32` samples and positions.
fn interp_oracle(method: Interpolation, window: &[f32], position: f32) -> f64 {
    let p = f64::from(position);
    let base = p.floor();
    let x = p - base;
    let [ym1, y0, y1, y2] = [-1.0, 0.0, 1.0, 2.0].map(|offset: f64| {
        (base + offset)
            .to_usize()
            .and_then(|index| window.get(index))
            .map_or(0.0, |sample| f64::from(*sample))
    });
    match method {
        Interpolation::Linear => (y1 - y0).mul_add(x, y0),
        Interpolation::Quadratic => {
            let slope = 0.5 * (y1 - ym1);
            let curve = 0.5 * (y1 - 2.0 * y0 + ym1);
            curve.mul_add(x, slope).mul_add(x, y0)
        }
        Interpolation::Hermite => {
            let c1 = 0.5 * (y1 - ym1);
            let c2 = ym1 - 2.5 * y0 + 2.0 * y1 - 0.5 * y2;
            let c3 = 1.5 * (y0 - y1) + 0.5 * (y2 - ym1);
            c3.mul_add(x, c2).mul_add(x, c1).mul_add(x, y0)
        }
        Interpolation::Watte => {
            let outer = ym1 + y2;
            let c1 = 1.5 * y1 - 0.5 * (y0 + outer);
            let c2 = 0.5 * (outer - y0 - y1);
            c2.mul_add(x, c1).mul_add(x, y0)
        }
    }
}

#[kithara::test]
fn every_interpolation_backend_tracks_the_f64_oracle() {
    let backends: Vec<(&str, Interp)> = Vec::from([
        ("portable", portable::interpolate as Interp),
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        ("accelerate", accelerate::interpolate),
    ]);
    let window: Vec<f32> = signal(
        usize::from(INTERP_WINDOW).saturating_add(SPECIALS.len()),
        sine_440(),
    )
    .into_iter()
    .skip(SPECIALS.len())
    .collect();
    let peak = window
        .iter()
        .fold(0.0_f64, |peak, sample| peak.max(f64::from(sample.abs())));
    for method in INTERP_METHODS {
        let (before, after) = method.padding();
        let low = f64::from(before);
        let span = f64::from(INTERP_WINDOW) - f64::from(after) - low;
        let positions: Vec<f32> = (0..INTERP_POSITIONS)
            .filter_map(|step| {
                (f64::from(step) / f64::from(INTERP_POSITIONS))
                    .mul_add(span, low)
                    .to_f32()
            })
            .collect();
        for (name, run) in &backends {
            let mut output = vec![UNWRITTEN; positions.len()];
            assert_eq!(
                run(method, &window, &positions, &mut output),
                Ok(positions.len()),
                "{name}: {method:?}"
            );
            let worst = positions
                .iter()
                .zip(&output)
                .fold(0.0_f64, |worst, (position, sample)| {
                    worst
                        .max((f64::from(*sample) - interp_oracle(method, &window, *position)).abs())
                });
            assert!(
                worst <= INTERP_PARITY * peak,
                "{name}: {method:?} off by {worst} of {peak}"
            );
        }
    }
}

/// Neighbours of opposite sign cancel in the Catmull-Rom coefficients; `f32`
/// arithmetic there loses more than the four epsilons the contract allows.
const CANCELLING_WINDOW: [f32; 4] = [-0.573_767_2, 0.718_489_6, -0.533_961_2, 0.338_097_85];
const CANCELLING_POSITION: f32 = 1.947_738_2;

#[kithara::test]
fn every_interpolation_backend_rounds_once_on_a_cancelling_window() {
    let backends: Vec<(&str, Interp)> = Vec::from([
        ("portable", portable::interpolate as Interp),
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        ("accelerate", accelerate::interpolate),
    ]);
    let peak = CANCELLING_WINDOW
        .iter()
        .fold(0.0_f64, |peak, sample| peak.max(f64::from(sample.abs())));
    for method in INTERP_METHODS {
        let (before, _) = method.padding();
        let positions = [f32::from(before), CANCELLING_POSITION];
        for (name, run) in &backends {
            let mut output = [UNWRITTEN; 2];
            assert_eq!(
                run(method, &CANCELLING_WINDOW, &positions, &mut output),
                Ok(positions.len()),
                "{name}: {method:?}"
            );
            for (position, sample) in positions.iter().zip(output) {
                let error = (f64::from(sample)
                    - interp_oracle(method, &CANCELLING_WINDOW, *position))
                .abs();
                assert!(
                    error <= INTERP_PARITY * peak,
                    "{name}: {method:?} at {position} off by {error} of {peak}"
                );
            }
        }
    }
}

/// `got` within `ulps` steps of the exact `want`, or both `NaN`.
fn within_ulps(got: f32, want: f64, ulps: usize) -> bool {
    let (mut low, mut high) = (got, got);
    for _ in 0..ulps {
        (low, high) = (low.next_down(), high.next_up());
    }
    (got.is_nan() && want.is_nan()) || (f64::from(low)..=f64::from(high)).contains(&want)
}

/// `values` with the special-value table shifted by one slot, so a binary
/// kernel meets every special against its neighbour, not against itself.
fn shifted(mut values: Vec<f32>) -> Vec<f32> {
    let shift = values.len().min(1);
    values.rotate_left(shift);
    values
}

/// `size` signal slots followed by two `UNWRITTEN` slots, so a kernel
/// that writes past its prefix shows.
fn padded_signal(size: usize, prepared: &[f32]) -> Vec<f32> {
    let mut values = signal(size, prepared);
    values.extend([UNWRITTEN; 2]);
    values
}

/// Asserts the returned count, every value of the prefix through `holds`,
/// and the untouched slots past it.
fn assert_prefix(
    context: &str,
    count: usize,
    output: &[f32],
    size: usize,
    holds: impl Fn(usize, f32) -> bool,
) {
    assert_eq!(count, size, "{context}: count");
    let (head, tail) = output.split_at(size);
    for (index, got) in head.iter().enumerate() {
        assert!(holds(index, *got), "{context}: {got} at {index}");
    }
    assert!(
        tail.iter()
            .all(|value| value.to_bits() == UNWRITTEN.to_bits()),
        "{context}: wrote past {size}"
    );
}

fn float(value: usize) -> f32 {
    value.to_f32().expect("test indices are finite")
}

/// Integer ramps are exact at every level, from every special start.
#[kithara::test]
fn ramp_counts_exactly_at_every_level() {
    for (name, ramp) in ramps() {
        for start in SPECIALS.into_iter().chain([5.0, -7.0]) {
            for step in [1.0, -3.0] {
                for size in SIZES {
                    for offset in OFFSETS {
                        let mut output =
                            vec![UNWRITTEN; size.saturating_add(offset).saturating_add(2)];
                        let region = output.split_at_mut(offset).1;
                        let (head, _) = region.split_at_mut(size);
                        let count = ramp(head, start, step);
                        assert_prefix(
                            &format!("{name}: {start} by {step}, size {size}, offset {offset}"),
                            count,
                            region,
                            size,
                            |slot, got| same(got, start + float(slot) * step),
                        );
                    }
                }
            }
        }
    }
}

#[kithara::test]
fn div_matches_ieee_division_at_every_level() {
    let fallback = |samples: &mut [f32], divisor: f32| dispatch!(Level::fallback(), simd => simd::div_kernel(simd, samples, divisor));
    for (name, div) in [
        ("native", simd::div as fn(&mut [f32], f32) -> usize),
        ("fallback", fallback),
    ] {
        for divisor in [3.0, -0.7].into_iter().chain(SPECIALS) {
            for size in SIZES {
                for offset in OFFSETS {
                    let input = padded_signal(size.saturating_add(offset), sine_440());
                    let mut output = input.clone();
                    let region = output.split_at_mut(offset).1;
                    let count = div(region.split_at_mut(size).0, divisor);
                    let input = input.split_at(offset).1;
                    assert_prefix(
                        &format!("{name}: / {divisor}, size {size}, offset {offset}"),
                        count,
                        region,
                        size,
                        |slot, got| input.get(slot).is_some_and(|x| same(got, x / divisor)),
                    );
                }
            }
        }
    }
}

#[kithara::test]
fn affine_rounds_within_one_ulp_on_every_backend() {
    for row in vectors() {
        let name = row.name;
        for (scale, offset_value) in [(0.75, -0.25), (-3.0, 0.5)] {
            for size in SIZES {
                for offset in OFFSETS {
                    let input = padded_signal(size.saturating_add(offset), sine_440());
                    let mut output = input.clone();
                    let region = output.split_at_mut(offset).1;
                    let count = (row.affine)(region.split_at_mut(size).0, scale, offset_value);
                    let input = input.split_at(offset).1;
                    assert_prefix(
                        &format!("{name}: *{scale} + {offset_value}, size {size}, offset {offset}"),
                        count,
                        region,
                        size,
                        |slot, got| {
                            input.get(slot).is_some_and(|x| {
                                let exact = f64::from(*x)
                                    .mul_add(f64::from(scale), f64::from(offset_value));
                                within_ulps(got, exact, 1)
                            })
                        },
                    );
                }
            }
        }
    }
}

#[kithara::test]
fn sub_rounds_within_one_ulp_on_every_backend() {
    for row in vectors() {
        let name = row.name;
        for size in SIZES {
            for offset in OFFSETS {
                let x = padded_signal(size.saturating_add(offset), sine_440());
                let y = shifted(signal(size.saturating_add(offset), sawtooth()));
                let mut output = x.clone();
                let region = output.split_at_mut(offset).1;
                let count = (row.sub)(region, y.split_at(offset).1);
                let (x, y) = (x.split_at(offset).1, y.split_at(offset).1);
                assert_prefix(
                    &format!("{name}: size {size}, offset {offset}"),
                    count,
                    region,
                    size,
                    |slot, got| {
                        let (Some(x), Some(y)) = (x.get(slot), y.get(slot)) else {
                            return false;
                        };
                        within_ulps(got, f64::from(*x) - f64::from(*y), 1)
                    },
                );
            }
        }
    }
}

#[kithara::test]
fn scale_add_rounds_within_one_ulp_on_every_backend() {
    for row in vectors() {
        let name = row.name;
        for gain in [0.7, -1.5] {
            for size in SIZES {
                for offset in OFFSETS {
                    let x = shifted(signal(size.saturating_add(offset), sine_440()));
                    let acc = padded_signal(size.saturating_add(offset), sawtooth());
                    let mut output = acc.clone();
                    let region = output.split_at_mut(offset).1;
                    let count = (row.scale_add)(x.split_at(offset).1, gain, region);
                    let (x, acc) = (x.split_at(offset).1, acc.split_at(offset).1);
                    assert_prefix(
                        &format!("{name}: *{gain}, size {size}, offset {offset}"),
                        count,
                        region,
                        size,
                        |slot, got| {
                            let (Some(x), Some(acc)) = (x.get(slot), acc.get(slot)) else {
                                return false;
                            };
                            within_ulps(
                                got,
                                f64::from(*x).mul_add(f64::from(gain), f64::from(*acc)),
                                1,
                            )
                        },
                    );
                }
            }
        }
    }
}

#[kithara::test]
fn mul_add_rounds_within_one_ulp_at_every_level() {
    let descending: Vec<_> = (0..sawtooth().len())
        .map(|frame| f32::from(Wave::SawtoothDescending.sample(frame, RATE)) / 32_768.0)
        .collect();
    for (name, mul_add) in mul_adds() {
        for size in SIZES {
            for offset in OFFSETS {
                let x = signal(size.saturating_add(offset), sine_440());
                let y = shifted(signal(
                    size.saturating_add(offset).saturating_add(1),
                    &descending,
                ));
                let acc = padded_signal(size.saturating_add(offset), sawtooth());
                let mut output = acc.clone();
                let region = output.split_at_mut(offset).1;
                let count = mul_add(x.split_at(offset).1, y.split_at(offset).1, region);
                let (x, y, acc) = (
                    x.split_at(offset).1,
                    y.split_at(offset).1,
                    acc.split_at(offset).1,
                );
                assert_prefix(
                    &format!("{name}: size {size}, offset {offset}"),
                    count,
                    region,
                    size,
                    |slot, got| {
                        let (Some(x), Some(y), Some(acc)) =
                            (x.get(slot), y.get(slot), acc.get(slot))
                        else {
                            return false;
                        };
                        within_ulps(
                            got,
                            f64::from(*x).mul_add(f64::from(*y), f64::from(*acc)),
                            1,
                        )
                    },
                );
            }
        }
    }
}

/// `(1 + 2^-12)*(1 + 2^-12) - (1 + 2^-11)` is exactly 2^-24, but the product
/// rounded alone cancels to `0.0`: a kernel that rounds the product before
/// the add loses the whole result.
const CANCELLING_TRIPLE: [f32; 3] = [1.000_244_1, 1.000_244_1, -1.000_488_3];

#[kithara::test]
fn every_multiply_add_rounds_once_on_a_cancelling_triple() {
    let [x, y, acc] = CANCELLING_TRIPLE;
    let exact = f64::from(x).mul_add(f64::from(y), f64::from(acc));
    let assert_rounds_once = |context: &str, output: &[f32]| {
        for (index, got) in output.iter().enumerate() {
            assert!(within_ulps(*got, exact, 1), "{context}: {got} at {index}");
        }
    };
    for size in SIZES {
        for row in vectors() {
            let name = row.name;
            let mut affine = vec![x; size];
            (row.affine)(&mut affine, y, acc);
            assert_rounds_once(&format!("{name}: affine, size {size}"), &affine);
            let mut scale_add = vec![acc; size];
            (row.scale_add)(&vec![x; size], y, &mut scale_add);
            assert_rounds_once(&format!("{name}: scale_add, size {size}"), &scale_add);
        }
        for (name, mul_add) in mul_adds() {
            let mut output = vec![acc; size];
            mul_add(&vec![x; size], &vec![y; size], &mut output);
            assert_rounds_once(&format!("{name}: mul_add, size {size}"), &output);
        }
    }
}

#[kithara::test]
fn add_rounds_within_one_ulp_on_every_backend() {
    for row in vectors() {
        let name = row.name;
        for size in SIZES {
            for offset in OFFSETS {
                let x = shifted(signal(size.saturating_add(offset), sine_440()));
                let acc = padded_signal(size.saturating_add(offset), sawtooth());
                let mut output = acc.clone();
                let region = output.split_at_mut(offset).1;
                let count = (row.add)(x.split_at(offset).1, region);
                let (x, acc) = (x.split_at(offset).1, acc.split_at(offset).1);
                assert_prefix(
                    &format!("{name}: size {size}, offset {offset}"),
                    count,
                    region,
                    size,
                    |slot, got| {
                        let (Some(x), Some(acc)) = (x.get(slot), acc.get(slot)) else {
                            return false;
                        };
                        within_ulps(got, f64::from(*acc) + f64::from(*x), 1)
                    },
                );
            }
        }
    }
}

/// A target shorter than its inputs bounds the prefix on every backend.
#[kithara::test]
fn a_short_target_bounds_every_elementwise_kernel() {
    let input = signal(9, sine_440());
    for row in vectors() {
        let name = row.name;
        let mut target = [0.0_f32; 5];
        assert_eq!((row.sub)(&mut target, &input), 5, "{name}: sub");
        assert_eq!(
            (row.scale_add)(&input, 0.5, &mut target),
            5,
            "{name}: scale_add"
        );
        assert_eq!((row.add)(&input, &mut target), 5, "{name}: add");
    }
    for (name, mul_add) in mul_adds() {
        let mut target = [0.0_f32; 5];
        assert_eq!(mul_add(&input, &input, &mut target), 5, "{name}: mul_add");
    }
}

/// Edges first, then a band just below the incoming edge where the cosine
/// side tends to zero, then an even grid over `[-1/8, 9/8]`.
fn fade_positions() -> Vec<f32> {
    const GRID: u16 = 4_096;
    let start = 0.000_01_f32;
    let end = 0.999_99_f32;
    let edges = [
        0.0,
        -0.0,
        start,
        start.next_up(),
        start.next_down(),
        end,
        end.next_down(),
        end.next_up(),
        1.0,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
    ];
    let band = core::iter::successors(Some(end.next_down()), |u: &f32| {
        Some(u.next_down()).filter(|u| *u > 0.999_98)
    });
    let grid = (0..=GRID).map(|step| (f32::from(step) / f32::from(GRID)).mul_add(1.25, -0.125));
    edges.into_iter().chain(band).chain(grid).collect()
}

/// `(outgoing, incoming)` of `curve` at `u` in `f64`, at the angle firewheel
/// rounds to `f32`.
fn exact_gains(curve: FadeCurve, u: f32) -> (f64, f64) {
    let angle = f64::from(core::f32::consts::FRAC_PI_2 * u);
    let u = f64::from(u);
    match curve {
        FadeCurve::EqualPower3dB => (angle.cos(), angle.sin()),
        FadeCurve::EqualPower6dB => (angle.cos().powi(2), angle.sin().powi(2)),
        FadeCurve::SquareRoot => ((1.0 - u).sqrt(), u.sqrt()),
        FadeCurve::Linear => (1.0 - u, u),
    }
}

/// Whether `got` is the outgoing (or incoming) gain of `curve` at `u`:
/// exact at the edges, within the curve's level between them.
fn gain_holds(curve: FadeCurve, outgoing: bool, u: f32, got: f32) -> bool {
    // At an edge, whether this side is the one at full level.
    let full = if u <= 0.000_01 {
        Some(outgoing)
    } else if u >= 0.999_99 {
        Some(!outgoing)
    } else {
        None
    };
    full.map_or_else(
        || {
            let (from, to) = exact_gains(curve, u);
            within_ulps(got, if outgoing { from } else { to }, gain_ulps(curve))
        },
        |full| got.to_bits() == f32::from(u8::from(full)).to_bits(),
    )
}

/// The spec's precision level per curve: 2 ulp for sin/cos, 1 for the rest.
fn gain_ulps(curve: FadeCurve) -> usize {
    match curve {
        FadeCurve::EqualPower3dB | FadeCurve::EqualPower6dB => 2,
        FadeCurve::SquareRoot | FadeCurve::Linear => 1,
    }
}

const CURVES: [FadeCurve; 4] = [
    FadeCurve::EqualPower3dB,
    FadeCurve::EqualPower6dB,
    FadeCurve::SquareRoot,
    FadeCurve::Linear,
];

#[kithara::test]
fn in_place_product_preserves_ieee_prefixes_at_every_level() {
    type Product = fn(&mut [f32], &[f32]) -> usize;
    let fallback: Product = |samples, gains| dispatch!(Level::fallback(), simd => simd::multiply_in_place_kernel(simd, samples, gains));
    let mut products: Vec<(&str, Product)> = vec![
        ("native", portable::multiply_in_place),
        ("fallback", fallback),
        ("public", crate::multiply_in_place),
    ];
    if cfg!(any(target_os = "macos", target_os = "ios")) {
        products.push(("platform", platform::multiply_in_place));
    }
    for (name, product) in products {
        for size in SIZES {
            for offset in OFFSETS {
                for gain in SPECIALS.into_iter().chain([1.0, -0.7, 1.000_000_1]) {
                    let input = padded_signal(size.saturating_add(offset), sine_440());
                    for shorter in [false, true] {
                        let count = if shorter {
                            size.saturating_sub(1)
                        } else {
                            size
                        };
                        let gains = vec![gain; count];
                        let mut actual = input.clone();
                        assert_eq!(
                            product(actual.split_at_mut(offset).1, &gains),
                            count,
                            "{name}: short side controls the FFI count"
                        );
                        for (index, (&got, &original)) in actual.iter().zip(&input).enumerate() {
                            if (offset..offset.checked_add(count).unwrap()).contains(&index) {
                                assert!(same(got, original * gain), "{name}: {index}, gain {gain}");
                            } else {
                                assert_eq!(
                                    got.to_bits(),
                                    original.to_bits(),
                                    "{name}: tail {index}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

type Gains = fn(FadeCurve, &[f32], &mut [f32], &mut [f32]) -> usize;

#[kithara::test]
fn fade_gains_track_the_f64_curves_at_every_level() {
    let fallback: Gains = |curve, positions, outgoing, incoming| dispatch!(Level::fallback(), simd => fade::gains_kernel(simd, curve, positions, outgoing, incoming));
    let positions = fade_positions();
    for (name, gains) in [("native", fade::gains as Gains), ("fallback", fallback)] {
        for curve in CURVES {
            for size in SIZES.into_iter().chain([positions.len()]) {
                for offset in OFFSETS {
                    let Some(window) = positions.get(offset..offset.saturating_add(size)) else {
                        continue;
                    };
                    let mut outgoing = vec![UNWRITTEN; size.saturating_add(2)];
                    let mut incoming = outgoing.clone();
                    let count = gains(curve, window, &mut outgoing, &mut incoming);
                    let context = format!("{name}: {curve:?}, size {size}, offset {offset}");
                    for (side, output) in [(true, &outgoing), (false, &incoming)] {
                        assert_prefix(
                            &format!("{context}, outgoing {side}"),
                            count,
                            output,
                            size,
                            |slot, got| {
                                window
                                    .get(slot)
                                    .is_some_and(|u| gain_holds(curve, side, *u, got))
                            },
                        );
                    }
                }
            }
        }
        let mut short = [UNWRITTEN; 3];
        assert_eq!(
            gains(
                FadeCurve::Linear,
                &positions,
                &mut [UNWRITTEN; 8],
                &mut short
            ),
            3,
            "{name}: the shortest side bounds the prefix"
        );
    }
}
