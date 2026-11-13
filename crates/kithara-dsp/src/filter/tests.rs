use std::num::{NonZeroU32, NonZeroUsize};

use kithara_test_fixtures::portable::sine_440;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{
    Biquad, Coefficients, FilterError, Hertz, Response, StateVariableFilter, SvfDesign, Type,
    decay_frames,
};

const RATE: f64 = 48_000.0;
const Q: f64 = std::f64::consts::FRAC_1_SQRT_2;
const TWO: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);
/// RBJ cookbook low-pass at 1 kHz, 48 kHz, `Q = 1/√2`, computed in `f64`.
const REFERENCE: [f64; 5] = [
    0.003_916_126_660_547_383,
    0.007_832_253_321_094_766,
    0.003_916_126_660_547_383,
    -1.815_341_082_704_568,
    0.831_005_589_346_757_6,
];
/// A settled filter matches the long-run steady state this closely.
const SETTLED: f32 = 1.0e-6;

fn design(sample_rate: f64, cutoff: f64, q: f64) -> Coefficients<f64> {
    Coefficients::from_params(
        Type::LowPass,
        Hertz::from_hz(sample_rate).expect("positive rate"),
        Hertz::from_hz(cutoff).expect("positive cutoff"),
        q,
    )
    .expect("valid low-pass")
}

fn low_pass(cutoff: f64) -> Coefficients<f64> {
    design(RATE, cutoff, Q)
}

fn stereo(frames: usize) -> Vec<Vec<f32>> {
    let plane = sine_440()
        .get(..frames)
        .expect("prepared frame extent")
        .to_vec();
    vec![plane.clone(), plane]
}

fn built(cutoff: f64) -> Biquad {
    let mut filter = Biquad::new(TWO, NonZeroUsize::MIN).expect("filter builds");
    filter
        .retune(0, low_pass(cutoff))
        .expect("section 0 exists");
    filter
}

fn bits(planes: &[Vec<f32>]) -> Vec<Vec<u32>> {
    planes
        .iter()
        .map(|plane| plane.iter().map(|sample| sample.to_bits()).collect())
        .collect()
}

#[kithara::test]
fn low_pass_matches_the_cookbook_reference() {
    let Coefficients { a1, a2, b0, b1, b2 } = low_pass(1_000.0);
    for (actual, expected) in [b0, b1, b2, a1, a2].into_iter().zip(REFERENCE) {
        assert!(
            (actual - expected).abs() <= 4.0 * f64::EPSILON * expected.abs(),
            "{actual} differs from {expected}"
        );
    }
}

/// A section runs in `f32`: every value must be finite there and both poles
/// must lie inside the unit circle.
#[kithara::test]
fn retune_rejects_a_section_that_is_not_finite_or_stable_in_f32() {
    let valid = low_pass(1_000.0);
    let mut filter = Biquad::new(TWO, NonZeroUsize::MIN).expect("filter builds");
    for section in [
        Coefficients {
            a1: -2.0,
            a2: 1.0,
            ..valid
        },
        Coefficients {
            b0: f64::NAN,
            ..valid
        },
        Coefficients {
            b1: 1.0e40,
            ..valid
        },
    ] {
        assert_eq!(
            filter.retune(0, section),
            Err(FilterError::Parameters),
            "{section:?}"
        );
    }
}

#[kithara::test]
fn settle_reaches_the_long_run_steady_state() {
    let levels = [0.5_f32, -0.25];
    let mut steady = built(1_000.0);
    let mut long_run: Vec<Vec<f32>> = levels.iter().map(|level| vec![*level; 16_384]).collect();
    steady
        .process(&mut long_run, 0..16_384)
        .expect("shape matches");
    let mut settled = built(1_000.0);
    settled.settle(&levels).expect("one level per channel");
    let mut planes: Vec<Vec<f32>> = levels.iter().map(|level| vec![*level; 64]).collect();
    settled.process(&mut planes, 0..64).expect("shape matches");

    for (plane, reference) in planes.iter().zip(&long_run) {
        let target = reference.last().copied().expect("long run is non-empty");
        assert!(
            plane
                .iter()
                .all(|sample| (sample - target).abs() <= SETTLED),
            "settled output left {target}: {plane:?}"
        );
    }
}

#[kithara::test]
fn silence_resets_to_the_state_of_a_fresh_filter() {
    let mut used = built(1_000.0);
    let mut burst = stereo(256);
    used.process(&mut burst, 0..256).expect("shape matches");
    let mut quiet = vec![vec![0.0_f32; 1_024]; 2];
    used.process(&mut quiet, 0..1_024).expect("shape matches");
    let mut silence = vec![vec![0.0_f32; 64]; 2];
    used.process(&mut silence, 0..64).expect("shape matches");

    let mut after = stereo(256);
    used.process(&mut after, 0..256).expect("shape matches");
    let mut fresh = stereo(256);
    built(1_000.0)
        .process(&mut fresh, 0..256)
        .expect("shape matches");
    assert_eq!(bits(&after), bits(&fresh));
}

/// At a quarter of the rate with `Q = 1/2` the section is the FIR
/// `(1 + 2z⁻¹ + z⁻²) / 4`: after `[1, −½]` the next output is `0` while the
/// section still remembers `−½`, so a one-frame call looks silent.
#[kithara::test]
fn a_quiet_call_shorter_than_the_section_memory_keeps_the_state() {
    let fir = design(RATE, RATE / 4.0, 0.5);
    let mut split = Biquad::new(TWO, NonZeroUsize::MIN).expect("filter builds");
    let mut whole = Biquad::new(TWO, NonZeroUsize::MIN).expect("filter builds");
    split.retune(0, fir).expect("section 0 exists");
    whole.retune(0, fir).expect("section 0 exists");
    let mut left = vec![vec![1.0_f32, -0.5, 0.0, 0.0]; 2];
    let mut right = left.clone();
    for range in [0..2, 2..3, 3..4] {
        split.process(&mut left, range).expect("shape matches");
    }
    whole.process(&mut right, 0..4).expect("shape matches");
    assert_eq!(bits(&left), bits(&right));
}

#[kithara::test]
fn an_empty_range_keeps_the_state() {
    let mut probed = built(1_000.0);
    let mut plain = built(1_000.0);
    let mut left = stereo(256);
    let mut right = stereo(256);
    probed.process(&mut left, 0..128).expect("shape matches");
    probed
        .process(&mut left, 128..128)
        .expect("an empty range is a no-op");
    probed.process(&mut left, 128..256).expect("shape matches");
    plain.process(&mut right, 0..256).expect("shape matches");
    assert_eq!(bits(&left), bits(&right));
}

#[kithara::test]
fn reset_clears_history_and_keeps_the_design() {
    let mut filter = built(1_000.0);
    let mut warmup = stereo(256);
    filter.process(&mut warmup, 0..256).expect("shape matches");
    filter.reset();
    let mut actual = stereo(256);
    let mut expected = actual.clone();
    filter.process(&mut actual, 0..256).expect("shape matches");
    built(1_000.0)
        .process(&mut expected, 0..256)
        .expect("shape matches");
    assert_eq!(bits(&actual), bits(&expected));
}

#[kithara::test]
fn reset_restarts_the_quiet_frame_sequence() {
    let mut filter = Biquad::new(NonZeroUsize::MIN, NonZeroUsize::MIN).expect("filter builds");
    filter
        .retune(
            0,
            Coefficients {
                b0: 1.0,
                b1: 0.5,
                b2: 0.0,
                a1: 0.0,
                a2: 0.0,
            },
        )
        .expect("exact FIR");
    let amplitude = f32::from_bits(0x3000_0000);
    filter
        .process(&mut [vec![amplitude]], 0..1)
        .expect("one quiet frame");
    filter.reset();
    let mut actual = vec![vec![amplitude, 0.0, 0.0]];
    filter.process(&mut actual, 0..3).expect("fresh sequence");
    let expected = vec![vec![amplitude, amplitude / 2.0, 0.0]];
    assert_eq!(bits(&actual), bits(&expected));
}

#[kithara::test]
fn samples_outside_the_range_keep_their_bits() {
    let mut filter = built(1_000.0);
    let original = stereo(64);
    let mut planes = original.clone();
    filter.process(&mut planes, 16..48).expect("shape matches");
    for (plane, source) in planes.iter().zip(&original) {
        let outside = |values: &[f32]| -> Vec<u32> {
            values
                .iter()
                .take(16)
                .chain(values.iter().skip(48))
                .map(|sample| sample.to_bits())
                .collect()
        };
        assert_eq!(outside(plane), outside(source));
    }
}

#[kithara::test]
fn a_mismatched_shape_is_an_error() {
    let mut filter = built(1_000.0);
    let mut one = vec![vec![0.0_f32; 8]];
    let mut short = vec![vec![0.0_f32; 8], vec![0.0_f32; 4]];
    let other = Biquad::new(NonZeroUsize::MIN, NonZeroUsize::MIN).expect("filter builds");

    assert_eq!(filter.process(&mut one, 0..8), Err(FilterError::Shape));
    assert_eq!(filter.process(&mut short, 0..8), Err(FilterError::Shape));
    assert_eq!(filter.retune(1, low_pass(1_000.0)), Err(FilterError::Shape));
    assert_eq!(filter.settle(&[0.0]), Err(FilterError::Shape));
    assert_eq!(filter.copy_state(&other), Err(FilterError::Shape));
}

#[kithara::test]
#[case(1)]
#[case(3)]
fn recursive_samples_and_state_do_not_depend_on_chunk_boundaries(#[case] channels: usize) {
    const FRAMES: usize = 257;
    let source = sine_440()
        .get(..FRAMES)
        .expect("fixture covers requested frames")
        .to_vec();
    for sections in [1, 2] {
        let build = || {
            let mut filter = Biquad::new(
                NonZeroUsize::new(channels).expect("positive channels"),
                NonZeroUsize::new(sections).expect("positive sections"),
            )
            .expect("filter builds");
            for (section, cutoff) in [1_000.0, 6_000.0].into_iter().take(sections).enumerate() {
                filter
                    .retune(section, low_pass(cutoff))
                    .expect("valid section");
            }
            filter
        };
        let mut whole = build();
        let mut expected = vec![source.clone(); channels];
        whole
            .process(&mut expected, 0..FRAMES)
            .expect("whole input");
        let mut expected_suffix = vec![vec![0.125; 17]; channels];
        whole
            .process(&mut expected_suffix, 0..17)
            .expect("following state");
        for chunk in [1, 7, 97] {
            let mut split = build();
            let mut actual = vec![source.clone(); channels];
            for start in (0..FRAMES).step_by(chunk) {
                let end = start
                    .checked_add(chunk)
                    .expect("fixture chunk endpoint fits usize")
                    .min(FRAMES);
                split.process(&mut actual, start..end).expect("input chunk");
            }
            assert_eq!(
                bits(&actual),
                bits(&expected),
                "{channels}ch/{sections}sec/{chunk}frames"
            );
            let mut suffix = vec![vec![0.125; 17]; channels];
            split.process(&mut suffix, 0..17).expect("following state");
            assert_eq!(bits(&suffix), bits(&expected_suffix), "following state");
        }
    }
}

#[kithara::test]
#[case::quiet_threshold(0x3000_0000)]
#[case::memory_floor(0x0d00_0000)]
fn quiet_fir_history_is_independent_of_chunk_boundaries(#[case] amplitude_bits: u32) {
    let build = || {
        let mut filter = Biquad::new(NonZeroUsize::MIN, NonZeroUsize::MIN).expect("filter builds");
        filter
            .retune(
                0,
                Coefficients {
                    b0: 1.0,
                    b1: 0.5,
                    b2: 0.0,
                    a1: 0.0,
                    a2: 0.0,
                },
            )
            .expect("exact FIR");
        filter
    };
    let amplitude = f32::from_bits(amplitude_bits);
    let input = vec![vec![amplitude, amplitude, 0.0]];
    let mut expected = input.clone();
    build().process(&mut expected, 0..3).expect("whole input");
    for chunk in [1, 2] {
        let mut split = build();
        let mut actual = input.clone();
        for start in (0_usize..3).step_by(chunk) {
            let end = start
                .checked_add(chunk)
                .expect("fixture chunk endpoint fits usize")
                .min(3);
            split.process(&mut actual, start..end).expect("quiet chunk");
        }
        assert_eq!(bits(&actual), bits(&expected));
    }
    let mut seeded = build();
    seeded
        .process(&mut [vec![amplitude]], 0..1)
        .expect("one quiet frame");
    let mut copied = build();
    copied.copy_state(&seeded).expect("same shape");
    let mut expected = vec![vec![amplitude, 0.0]];
    let mut actual = expected.clone();
    seeded
        .process(&mut expected, 0..2)
        .expect("source continuation");
    copied
        .process(&mut actual, 0..2)
        .expect("copied continuation");
    assert_eq!(bits(&actual), bits(&expected));
}

#[kithara::test]
fn identity_sections_preserve_small_nonzero_output_bits() {
    let input = [1.0e-35_f32, -1.0e-35, 1.0e-30, -1.0e-30, 3.0e-31, -3.0e-31];
    for channels in [1, 2, 3, 4, 5, 8] {
        for block in [1, 2, 6] {
            let mut filter = Biquad::new(NonZeroUsize::new(channels).expect("channels"), TWO)
                .expect("identity sections");
            let mut planes = vec![input.to_vec(); channels];
            for start in (0..input.len()).step_by(block) {
                filter
                    .process(
                        &mut planes,
                        start..start.saturating_add(block).min(input.len()),
                    )
                    .expect("complete planes");
            }
            assert_eq!(
                bits(&planes),
                vec![input.map(f32::to_bits).to_vec(); channels]
            );
        }
    }
}

fn half_decay(channels: NonZeroUsize) -> Biquad {
    let mut filter =
        Biquad::new(channels, NonZeroUsize::MIN).expect("channel count and one section");
    filter
        .retune(
            0,
            Coefficients {
                a1: -0.5,
                a2: 0.0,
                b0: 1.0,
                b1: 0.0,
                b2: 0.0,
            },
        )
        .expect("stable exact half decay");
    filter
}

#[kithara::test]
fn copy_state_preserves_a_partial_quiet_run() {
    let amplitude = 2.0e-9_f32;
    let mut source = half_decay(NonZeroUsize::MIN);
    let mut prefix = vec![vec![amplitude, 0.0]];
    source.process(&mut prefix, 0..2).expect("prefix shape");
    assert_eq!(bits(&prefix), bits(&[vec![amplitude, amplitude * 0.5]]));
    let mut copied = half_decay(NonZeroUsize::MIN);
    copied.copy_state(&source).expect("matching shape");
    let mut original_tail = vec![vec![0.0; 3]];
    let mut copied_tail = original_tail.clone();
    source
        .process(&mut original_tail, 0..3)
        .expect("source tail");
    copied.process(&mut copied_tail, 0..3).expect("copied tail");
    let expected = vec![vec![amplitude * 0.25, 0.0, 0.0]];
    assert_eq!(bits(&original_tail), bits(&expected));
    assert_eq!(bits(&copied_tail), bits(&expected));
}

#[kithara::test]
fn nan_input_is_nonquiet_under_every_partition() {
    let mut whole = vec![vec![1.0, 0.0, f32::NAN, 1.0e-10, 1.0]];
    let mut split = whole.clone();
    half_decay(NonZeroUsize::MIN)
        .process(&mut whole, 0..5)
        .expect("whole stream");
    let mut filter = half_decay(NonZeroUsize::MIN);
    filter.process(&mut split, 0..2).expect("prefix");
    filter.process(&mut split, 2..5).expect("suffix");
    assert_eq!(bits(&whole), bits(&split));
    assert!(
        whole
            .first()
            .and_then(|plane| plane.get(4))
            .is_some_and(|sample| sample.is_nan())
    );
}

#[kithara::test]
#[case::conjugate_pair(12_000.0, 5.0 / 6.0, 0.5)]
#[case::real_poles(((1.0_f64 - 0.9) / (1.0 + 0.9)).atan() * 48_000.0 / std::f64::consts::PI, 0.5, 0.9)]
fn decay_frames_counts_the_slowest_pole_down_to_the_level(
    #[case] cutoff: f64,
    #[case] q: f64,
    #[case] radius: f64,
) {
    let level = 1e-6;
    let design = SvfDesign::new(SVF_RATE, cutoff, q, Response::LowPass).expect("design");
    let expected = (f64::ln(level) / radius.ln()).ceil();
    assert_eq!(decay_frames(design, level).to_f64(), Some(expected));
    let memoryless = SvfDesign::new(SVF_RATE, 12_000.0, 0.5, Response::HighPass).expect("design");
    assert_eq!(
        decay_frames(memoryless, level),
        1,
        "a filter without memory counts one frame"
    );
}

const SVF_RATE: NonZeroU32 = NonZeroU32::new(48_000).expect("nonzero rate");

fn variable(response: Response, planes: &mut [Vec<f32>], cutoffs: &[f64]) {
    let mut filter = StateVariableFilter::<2>::new();
    let frames = cutoffs.len();
    filter
        .process(planes, 0..frames, cutoffs, SVF_RATE, Q, response)
        .expect("valid state-variable run");
}

/// At fixed parameters the state-variable outputs are the RBJ responses.
#[kithara::test]
#[case(Response::LowPass, Type::LowPass)]
#[case(Response::HighPass, Type::HighPass)]
fn state_variable_outputs_match_the_cookbook_biquad(
    #[case] response: Response,
    #[case] kind: Type<f64>,
) {
    let frames = 4_096;
    let mut variable_planes = stereo(frames);
    variable(response, &mut variable_planes, &vec![1_000.0; frames]);
    let mut reference = stereo(frames);
    let mut biquad = Biquad::new(TWO, NonZeroUsize::MIN).expect("filter builds");
    biquad
        .retune(
            0,
            Coefficients::from_params(
                kind,
                Hertz::from_hz(RATE).expect("rate"),
                Hertz::from_hz(1_000.0).expect("cutoff"),
                Q,
            )
            .expect("design"),
        )
        .expect("section");
    biquad
        .process(&mut reference, 0..frames)
        .expect("reference");
    for (actual, expected) in variable_planes
        .iter()
        .flatten()
        .zip(reference.iter().flatten())
    {
        assert!(
            (actual - expected).abs() <= 1e-5,
            "{response:?}: {actual} against {expected}"
        );
    }
}

/// Zero input never grows the state, whatever the cutoff does on each frame.
#[kithara::test]
#[case(0.1)]
#[case(Q)]
#[case(10.0)]
fn zero_input_never_grows_under_per_frame_modulation(#[case] q: f64) {
    let mut filter = StateVariableFilter::<1>::new();
    let mut kick = vec![1.0_f32];
    filter
        .process(
            &mut [kick.as_mut_slice()],
            0..1,
            &[200.0],
            SVF_RATE,
            q,
            Response::LowPass,
        )
        .expect("kick");
    let frames = 48_000;
    let cutoffs: Vec<_> = (0..frames)
        .map(|frame| {
            let hz = if frame % 3 == 0 { 20.0 } else { 20_000.0 };
            (hz * (1.0 + f64::from(u32::try_from(frame % 7).expect("small")) / 8.0)).min(23_900.0)
        })
        .collect();
    let mut plane = vec![0.0_f32; frames];
    filter
        .process(
            &mut [plane.as_mut_slice()],
            0..frames,
            &cutoffs,
            SVF_RATE,
            q,
            Response::LowPass,
        )
        .expect("modulated decay");
    // `|y2| ≤ ‖y‖ ≤ ‖s‖` and the stored state starts at norm 2·|y(kick)| ≤ 2.
    assert!(
        plane
            .iter()
            .all(|sample| sample.is_finite() && sample.abs() <= 2.0)
    );
}

/// A minute of per-frame modulation keeps normalized PCM finite and bounded.
#[kithara::test]
#[case(Response::HighPass, Q)]
#[case(Response::HighPass, 10.0)]
#[case(Response::LowPass, 10.0)]
fn a_sixty_second_sweep_stays_bounded(#[case] response: Response, #[case] q: f64) {
    let frames = 60 * 48_000;
    let ratio = 8_000.0_f64 / 200.0;
    let cutoffs: Vec<_> = (0..frames)
        .map(|frame| {
            let progress = f64::from(u32::try_from(frame).expect("frame"))
                / f64::from(u32::try_from(frames - 1).expect("frames"));
            200.0 * ratio.powf(progress)
        })
        .collect();
    let mut plane: Vec<f32> = sine_440().iter().copied().cycle().take(frames).collect();
    let mut filter = StateVariableFilter::<1>::new();
    filter
        .process(
            &mut [plane.as_mut_slice()],
            0..frames,
            &cutoffs,
            SVF_RATE,
            q,
            response,
        )
        .expect("sweep");
    let peak = plane
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    assert!(peak.is_finite() && peak <= 20.0, "{peak}");
}

#[kithara::test]
fn state_variable_blocks_are_bit_identical_under_any_partition() {
    let frames = 1_027;
    let cutoffs: Vec<_> = (0..frames)
        .map(|frame| 200.0 + 7.0 * f64::from(u32::try_from(frame).expect("frame")))
        .collect();
    let mut whole = stereo(frames);
    variable(Response::HighPass, &mut whole, &cutoffs);
    let mut split = stereo(frames);
    let mut filter = StateVariableFilter::<2>::new();
    let mut start = 0;
    for length in [1, 7, 300, 3].into_iter().cycle() {
        if start == frames {
            break;
        }
        let end = start.saturating_add(length).min(frames);
        filter
            .process(
                &mut split,
                start..end,
                cutoffs.get(start..end).expect("block cutoffs"),
                SVF_RATE,
                Q,
                Response::HighPass,
            )
            .expect("block");
        start = end;
    }
    assert_eq!(bits(&split), bits(&whole));
}

/// Every plane of a `CHANNELS`-wide filter runs the one-channel kernel on its
/// own history, and a plane count other than `CHANNELS` is refused.
fn channels_are_independent_mono_filters<const CHANNELS: usize>() {
    let frames = 1_027;
    let cutoffs: Vec<_> = (0..frames)
        .map(|frame| 300.0 + 5.0 * f64::from(u32::try_from(frame).expect("frame")))
        .collect();
    let mut planes: Vec<Vec<f32>> = (1..=CHANNELS)
        .map(|channel| {
            let scale = f32::from(u8::try_from(channel).expect("channel")) / 8.0;
            [1.0, -1.0, 0.5, 0.0]
                .iter()
                .cycle()
                .take(frames)
                .map(|sample| sample * scale)
                .collect()
        })
        .collect();
    let mut expected = planes.clone();
    for plane in &mut expected {
        StateVariableFilter::<1>::new()
            .process(
                std::slice::from_mut(plane),
                0..frames,
                &cutoffs,
                SVF_RATE,
                Q,
                Response::LowPass,
            )
            .expect("mono kernel");
    }
    let mut filter = StateVariableFilter::<CHANNELS>::new();
    let mut short = vec![vec![0.25; frames]; CHANNELS.saturating_sub(1)];
    assert_eq!(
        filter.process(
            &mut short,
            0..frames,
            &cutoffs,
            SVF_RATE,
            Q,
            Response::LowPass
        ),
        Err(FilterError::Shape)
    );
    filter
        .process(
            &mut planes,
            0..frames,
            &cutoffs,
            SVF_RATE,
            Q,
            Response::LowPass,
        )
        .expect("wide kernel");
    assert_eq!(bits(&planes), bits(&expected));
}

#[kithara::test]
fn state_variable_channels_are_independent_at_one_two_and_six_channels() {
    channels_are_independent_mono_filters::<1>();
    channels_are_independent_mono_filters::<2>();
    channels_are_independent_mono_filters::<6>();
}

#[kithara::test]
fn state_variable_refusals_change_nothing_and_quiet_tails_reset() {
    let mut filter = StateVariableFilter::<2>::new();
    let mut planes = stereo(8);
    let before = bits(&planes);
    for (cutoffs, q, error) in [
        (vec![1_000.0; 7], Q, FilterError::Shape),
        (vec![f64::NAN; 8], Q, FilterError::Parameters),
        (vec![0.0; 8], Q, FilterError::Parameters),
        (vec![24_000.0; 8], Q, FilterError::Parameters),
        (vec![1_000.0; 8], 0.0, FilterError::Parameters),
        (vec![1_000.0; 8], f64::INFINITY, FilterError::Parameters),
    ] {
        assert_eq!(
            filter.process(&mut planes, 0..8, &cutoffs, SVF_RATE, q, Response::LowPass),
            Err(error)
        );
    }
    assert_eq!(bits(&planes), before);
    assert!(filter.is_reset());
    filter
        .process(
            &mut planes,
            0..8,
            &[1_000.0; 8],
            SVF_RATE,
            Q,
            Response::LowPass,
        )
        .expect("tone");
    assert!(!filter.is_reset());
    let mut silence = vec![vec![0.0; 2], vec![0.0; 2]];
    filter
        .process(
            &mut silence,
            0..2,
            &[20_000.0; 2],
            SVF_RATE,
            Q,
            Response::HighPass,
        )
        .expect("silence");
    assert!(!filter.is_reset(), "a ringing tail is not quiet");
    filter.reset();
    assert!(filter.is_reset());
    let mut zeros = vec![vec![0.0; 3], vec![0.0; 3]];
    filter
        .process(
            &mut zeros,
            0..3,
            &[1_000.0; 3],
            SVF_RATE,
            Q,
            Response::LowPass,
        )
        .expect("zeros");
    assert!(zeros.iter().flatten().all(|sample| sample.to_bits() == 0));
    assert!(filter.is_reset());
}
