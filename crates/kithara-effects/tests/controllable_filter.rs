#![forbid(unsafe_code)]

use std::num::{NonZeroU32, NonZeroUsize};

use kithara_config::ConfigOwner;
use kithara_dsp::filter::{FilterError, Response, StateVariableFilter};
use kithara_effects::filter::{Cutoff, FilterEffect, FilterKnob, FilterSettings, Kind, Q};
use kithara_signal::AudioSpec;
use kithara_test_fixtures::{
    signal::rms,
    unit_fixtures::{eq_sine_40, eq_sine_1000, eq_sine_10000},
};
use kithara_test_utils::kithara;

const RATE: f64 = 44_100.0;

fn spec(channels: u16) -> AudioSpec {
    AudioSpec::new(
        channels,
        NonZeroU32::new(44_100).expect("nonzero test rate"),
    )
}

fn knob(value: f64) -> FilterKnob {
    FilterKnob::checked(value).expect("knob in range")
}

fn cutoff(hz: f64) -> Cutoff {
    Cutoff::checked(hz).expect("admitted cutoff")
}

/// Settings with the `kind` side running from `edge` to `extreme`.
fn range(kind: Kind, edge: Cutoff, extreme: Cutoff, q: Q) -> FilterSettings {
    let low = kind == Kind::LowPass;
    FilterSettings::builder()
        .maybe_low_edge(low.then_some(edge))
        .maybe_low_extreme(low.then_some(extreme))
        .maybe_high_edge((!low).then_some(edge))
        .maybe_high_extreme((!low).then_some(extreme))
        .q(q)
        .build()
        .expect("valid filter range")
}

/// Settings whose end of the `kind` side sits exactly at `hz`.
fn end_at(kind: Kind, hz: f64) -> FilterSettings {
    let edge = if kind == Kind::LowPass {
        cutoff(20_000.0)
    } else {
        Cutoff::MIN
    };
    range(kind, edge, cutoff(hz), Q::default())
}

/// The knob end of the `kind` side.
fn end(kind: Kind) -> f64 {
    match kind {
        Kind::LowPass => -1.0,
        Kind::HighPass => 1.0,
    }
}

fn filter(spec: AudioSpec, settings: FilterSettings) -> FilterEffect {
    FilterEffect::new(spec, settings).expect("prepared filter")
}

fn planes_of(channels: u16, input: &[f32]) -> Vec<Vec<f32>> {
    let width = usize::from(channels);
    assert_eq!(input.len() % width, 0, "fixture contains whole frames");
    (0..width)
        .map(|channel| input.iter().skip(channel).step_by(width).copied().collect())
        .collect()
}

fn interleaved(planes: &[Vec<f32>]) -> Vec<f32> {
    let frames = planes.first().map_or(0, Vec::len);
    (0..frames)
        .flat_map(|frame| planes.iter().map(move |plane| plane[frame]))
        .collect()
}

/// Runs `input` under the manual knob.
fn processed(
    filter: &mut FilterEffect,
    channels: u16,
    input: &[f32],
) -> Result<Vec<f32>, FilterError> {
    let mut planes = planes_of(channels, input);
    let frames = planes[0].len();
    filter.process(&mut planes, 0..frames)?;
    Ok(interleaved(&planes))
}

/// Runs `input` under a program of one knob value per frame.
fn programmed(filter: &mut FilterEffect, channels: u16, input: &[f32], knobs: &[f64]) -> Vec<f32> {
    let mut planes = planes_of(channels, input);
    let frames = planes[0].len();
    filter
        .process_program(&mut planes, 0..frames, knobs)
        .expect("program frames");
    interleaved(&planes)
}

/// Runs `input` under a program holding `value` on every frame.
fn held(filter: &mut FilterEffect, channels: u16, input: &[f32], value: f64) -> Vec<f32> {
    let frames = input.len() / usize::from(channels);
    programmed(filter, channels, input, &vec![value; frames])
}

fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|sample| sample.to_bits()).collect()
}

/// The kernel response and cutoff the settings give `value`.
fn response(settings: &FilterSettings, value: f64) -> (Response, f64) {
    let (kind, hz) = settings.response(value).expect("off center");
    let response = match kind {
        Kind::LowPass => Response::LowPass,
        Kind::HighPass => Response::HighPass,
    };
    (response, hz)
}

/// A manual glide is the program of the knob values it passes, in any block size.
#[kithara::test]
fn a_manual_glide_equals_the_program_of_its_knob_values_across_odd_blocks(eq_sine_1000: Vec<f32>) {
    let settings = FilterSettings::default();
    let mut whole = filter(spec(1), settings);
    let mut stepped = filter(spec(1), settings);
    let mut program = filter(spec(1), settings);
    for filter in [&mut whole, &mut stepped, &mut program] {
        held(filter, 1, &eq_sine_1000[..4096], -0.3);
    }
    whole.configure(knob(-0.7));
    stepped.configure(knob(-0.7));
    let input = &eq_sine_1000[4096..12_288];
    let expected = processed(&mut whole, 1, input).expect("whole block");
    let mut knobs = Vec::with_capacity(input.len());
    let mut split = Vec::with_capacity(input.len());
    for block in input.chunks(7) {
        split.extend_from_slice(&processed(&mut stepped, 1, &block[..1]).expect("one frame"));
        knobs.push(stepped.knob());
        if block.len() > 1 {
            let rest = processed(&mut stepped, 1, &block[1..]).expect("odd block");
            split.extend_from_slice(&rest);
            knobs.extend(std::iter::repeat_n(f64::NAN, rest.len()));
        }
    }
    assert_eq!(bits(&split), bits(&expected));
    assert_eq!(whole.knob(), -0.7);
    let mut frame_knobs = Vec::with_capacity(input.len());
    let mut probe = filter(spec(1), settings);
    held(&mut probe, 1, &eq_sine_1000[..4096], -0.3);
    probe.configure(knob(-0.7));
    for sample in input {
        processed(&mut probe, 1, std::slice::from_ref(sample)).expect("probe frame");
        frame_knobs.push(probe.knob());
    }
    assert!(
        knobs
            .iter()
            .zip(&frame_knobs)
            .all(|(a, b)| a.is_nan() || a == b)
    );
    assert_eq!(
        bits(&programmed(&mut program, 1, input, &frame_knobs)),
        bits(&expected)
    );
}

#[kithara::test]
fn the_center_passes_the_signal_bit_for_bit_and_holds_no_history() {
    let mut filter = filter(spec(2), FilterSettings::default());
    let input = [0.25, -0.5, 1e-30, f32::MIN_POSITIVE, -0.0, 0.75, 1.0, -1.0];
    assert_eq!(
        bits(&processed(&mut filter, 2, &input).expect("center")),
        bits(&input)
    );
    assert_eq!(bits(&held(&mut filter, 2, &input, 0.0)), bits(&input));
    assert!(filter.tail_exhausted());
}

#[kithara::test]
fn a_move_through_the_center_restarts_the_other_side_from_reset_state() {
    let mut swept = filter(spec(1), FilterSettings::default());
    let mut fresh = filter(spec(1), FilterSettings::default());
    let input = [0.25, -0.5, 0.75, 0.125].repeat(64);
    let low = held(&mut swept, 1, &input, -0.4);
    assert_ne!(bits(&low), bits(&input));
    assert!(!swept.tail_exhausted());
    assert_eq!(
        bits(&held(&mut swept, 1, &input, 0.4)),
        bits(&held(&mut fresh, 1, &input, 0.4))
    );
    assert_eq!(bits(&held(&mut swept, 1, &input, 0.0)), bits(&input));
    assert!(swept.tail_exhausted());
    let knobs: Vec<_> = (0..256)
        .map(|frame| if frame < 128 { -0.4 } else { 0.4 })
        .collect();
    let mut through = filter(spec(1), FilterSettings::default());
    let mut restarted = filter(spec(1), FilterSettings::default());
    let crossed = programmed(&mut through, 1, &input, &knobs);
    held(&mut restarted, 1, &input[..128], -0.4);
    restarted.reset();
    let after = held(&mut restarted, 1, &input[128..], 0.4);
    assert_eq!(bits(&crossed[128..]), bits(&after));
}

#[kithara::test]
fn program_blocks_are_bit_identical_under_two_partitions() {
    let settings = FilterSettings::default();
    let frames = 4_099;
    let input: Vec<f32> = [0.25, -0.5, 0.75, 0.125, -1.0]
        .iter()
        .copied()
        .cycle()
        .take(2 * frames)
        .collect();
    let knobs: Vec<_> = (0..frames)
        .map(|frame| -0.9 + 1.8 * f64::from(u32::try_from(frame).expect("frame")) / 4_098.0)
        .collect();
    let mut whole = filter(spec(2), settings);
    let mut planes = planes_of(2, &input);
    whole
        .process_program(&mut planes, 0..frames, &knobs)
        .expect("whole program");
    let mut split = filter(spec(2), settings);
    let mut parts = planes_of(2, &input);
    let mut start = 0;
    for length in [1, 7, 300, 13].into_iter().cycle() {
        if start == frames {
            break;
        }
        let stop = frames.min(start + length);
        split
            .process_program(&mut parts, start..stop, &knobs[start..stop])
            .expect("split program");
        start = stop;
    }
    assert_eq!(bits(&interleaved(&parts)), bits(&interleaved(&planes)));
}

/// A minute-long sweep is an ordinary program: the filter is stable for any
/// knob motion, so nothing about its length is checked or refused.
#[kithara::test]
fn a_sixty_second_sweep_from_200_to_8000_hz_is_accepted_and_stays_finite() {
    let rate = 48_000_u32;
    let settings = FilterSettings::default();
    let position = |hz: f64| (hz / 30.0).ln() / (10_000.0_f64 / 30.0).ln();
    let (from, to) = (position(200.0), position(8_000.0));
    for (value, hz) in [(from, 200.0), (to, 8_000.0)] {
        let (kind, actual) = settings.response(value).expect("high-pass side");
        assert_eq!(kind, Kind::HighPass);
        assert!((actual - hz).abs() < 1e-6 * hz, "{actual} Hz for {hz} Hz");
    }
    let frames = 60 * usize::try_from(rate).expect("rate");
    let mut effect = filter(
        AudioSpec::new(1, NonZeroU32::new(rate).expect("rate")),
        settings,
    );
    let step = (to - from) / f64::from(u32::try_from(frames - 1).expect("frames"));
    let mut plane: Vec<f32> = [1.0, -1.0, 0.5, -0.25].repeat(frames / 4);
    let mut knobs = vec![0.0; 4_800];
    for start in (0..frames).step_by(knobs.len()) {
        for (offset, knob) in knobs.iter_mut().enumerate() {
            *knob = from + step * f64::from(u32::try_from(start + offset).expect("frame"));
        }
        effect
            .process_program(
                &mut [plane.as_mut_slice()],
                start..start + knobs.len(),
                &knobs,
            )
            .expect("every program frame is accepted");
    }
    assert!(
        plane
            .iter()
            .all(|sample| sample.is_finite() && sample.abs() <= 4.0)
    );
}

#[kithara::test]
fn filter_rejects_invalid_parameters_and_ranges() {
    for hz in [0.0, -1.0, 19.0, 20_001.0, f64::NAN, f64::INFINITY] {
        assert!(Cutoff::checked(hz).is_none());
    }
    for q in [0.0, -1.0, 0.09, 10.01, f64::NAN, f64::INFINITY] {
        assert!(Q::checked(q).is_none());
    }
    for value in [-1.01, 1.01, f64::NAN, f64::INFINITY] {
        assert!(FilterKnob::checked(value).is_none());
    }
    assert!(
        FilterSettings::builder()
            .low_edge(cutoff(100.0))
            .low_extreme(cutoff(100.0))
            .build()
            .is_err()
    );
    assert!(
        FilterSettings::builder()
            .high_edge(cutoff(1_000.0))
            .high_extreme(cutoff(500.0))
            .build()
            .is_err()
    );
    assert_eq!(
        FilterEffect::new(spec(0), FilterSettings::default()).err(),
        Some(FilterError::Shape)
    );
    let low_rate = AudioSpec::new(1, NonZeroU32::new(32_000).expect("rate"));
    assert_eq!(
        FilterEffect::new(low_rate, end_at(Kind::HighPass, 20_000.0)).err(),
        Some(FilterError::Parameters)
    );
    let mut effect = filter(spec(1), FilterSettings::default());
    let mut plane = [0.25_f32; 4];
    let before = bits(&plane);
    assert_eq!(
        effect.process_program(&mut [plane.as_mut_slice()], 0..4, &[0.5, 0.5, 1.5, 0.5]),
        Err(FilterError::Parameters)
    );
    assert_eq!(
        effect.process_program(&mut [plane.as_mut_slice()], 0..4, &[0.5; 3]),
        Err(FilterError::Shape)
    );
    assert_eq!(bits(&plane), before);
    assert!(effect.tail_exhausted());
}

#[kithara::test]
fn filter_is_a_causal_planar_consumer_with_independent_channel_history() {
    let mut effect = filter(spec(2), end_at(Kind::HighPass, 1_000.0));
    let input = [0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let output = held(&mut effect, 2, &input, 1.0);
    assert_eq!(output.len(), input.len());
    assert_ne!(&output[..], &input);
    assert!(
        output
            .iter()
            .skip(1)
            .step_by(2)
            .all(|sample| sample.to_bits() == 0)
    );
    effect.reset();
    assert!(effect.tail_exhausted());
    assert!(
        held(&mut effect, 2, &[0.0; 8], 1.0)
            .iter()
            .all(|sample| sample.to_bits() == 0)
    );
}

#[kithara::test]
fn filter_keeps_history_across_odd_blocks_and_reset_clears_it() {
    let settings = end_at(Kind::LowPass, 1_000.0);
    let mut whole = filter(spec(1), settings);
    let mut split = filter(spec(1), settings);
    let mut input = vec![0.0; 257];
    input[0] = 0.25;
    input[97] = -0.125;
    let expected = held(&mut whole, 1, &input, -1.0);
    let mut actual = Vec::new();
    for block in input.chunks(7) {
        actual.extend_from_slice(&held(&mut split, 1, block, -1.0));
    }
    assert_eq!(bits(&actual), bits(&expected));
    split.reset();
    let silence = held(&mut split, 1, &[0.0; 16], -1.0);
    assert!(silence.iter().all(|sample| sample.to_bits() == 0));
    let replay = held(&mut split, 1, &input, -1.0);
    assert_eq!(bits(&replay), bits(&expected));
}

/// The mono 44.1 kHz fixture pins the Butterworth cutoff within 0.5 dB after
/// discarding 4096 samples of startup history.
#[kithara::test]
#[case(Kind::HighPass)]
#[case(Kind::LowPass)]
fn filter_matches_declared_corner_response(eq_sine_1000: Vec<f32>, #[case] kind: Kind) {
    let mut effect = filter(spec(1), end_at(kind, 1_000.0));
    let output = held(&mut effect, 1, &eq_sine_1000, end(kind));
    let measured_db = 20.0 * f64::from(rms(&output[4_096..]) / rms(&eq_sine_1000[4_096..])).log10();
    assert!(
        (measured_db + 3.010_299_956_64).abs() <= 0.5,
        "{kind:?}: {measured_db} dB"
    );
}

/// The DC group delay of a second-order Butterworth LP is 1/(2 Q tan(pi fc/fs)).
#[kithara::test]
fn low_pass_impulse_has_the_declared_causal_delay() {
    let hz = 200.0;
    let mut effect = filter(spec(1), end_at(Kind::LowPass, hz));
    let mut input = vec![0.0; 8192];
    let origin = 2048;
    input[origin] = 0.25;
    let output = held(&mut effect, 1, &input, -1.0);
    assert!(output[..origin].iter().all(|sample| *sample == 0.0));
    let (sum, moment) = output[origin..].iter().enumerate().fold(
        (0.0_f64, 0.0_f64),
        |(sum, moment), (frame, sample)| {
            let frame = f64::from(u32::try_from(frame).expect("bounded impulse"));
            let sample = f64::from(*sample);
            (sum + sample, moment + frame * sample)
        },
    );
    assert!((20.0 * (sum / 0.25).abs().log10()).abs() <= 0.5);
    let delay = moment / sum;
    let reference = std::f64::consts::FRAC_1_SQRT_2 / (std::f64::consts::PI * hz / RATE).tan();
    assert!(
        (delay - reference).abs() <= 0.1,
        "DC group delay: {delay} frames, causal reference {reference} frames"
    );
}

#[kithara::test]
fn refused_rate_and_plane_geometry_leave_format_and_history_untouched() {
    let settings = end_at(Kind::HighPass, 20_000.0);
    let mut effect = filter(spec(1), settings);
    let mut reference = filter(spec(1), settings);
    for filter in [&mut effect, &mut reference] {
        assert!(
            held(filter, 1, &[0.25, 0.0, 0.0], 1.0)
                .iter()
                .any(|sample| *sample != 0.0)
        );
    }
    let refused = AudioSpec::new(1, NonZeroU32::new(32_000).expect("changed rate"));
    let mut invalid_planes: [&mut [f32]; 0] = [];
    assert_eq!(
        effect.process(&mut invalid_planes, 0..0),
        Err(FilterError::Shape)
    );
    assert_eq!(
        effect.update_sample_rate(refused.sample_rate),
        Err(FilterError::Parameters)
    );
    assert!(!effect.accepts_spec(refused));
    assert!(effect.accepts_spec(spec(1)));
    let expected = held(&mut reference, 1, &[0.0; 3], 1.0);
    let actual = held(&mut effect, 1, &[0.0; 3], 1.0);
    assert!(expected.iter().any(|sample| *sample != 0.0));
    assert_eq!(bits(&actual), bits(&expected));
}

/// Compare settled fixture RMS against the bilinear Butterworth magnitude.
#[kithara::test]
#[case(Kind::LowPass, 40.0, eq_sine_40())]
#[case(Kind::LowPass, 10_000.0, eq_sine_10000())]
#[case(Kind::HighPass, 40.0, eq_sine_40())]
#[case(Kind::HighPass, 10_000.0, eq_sine_10000())]
fn filter_matches_pass_and_stop_reference_regions(
    #[case] kind: Kind,
    #[case] hz: f64,
    #[case] input: Vec<f32>,
) {
    let mut effect = filter(spec(1), end_at(kind, 1_000.0));
    let output = held(&mut effect, 1, &input, end(kind));
    let measured = 20.0 * f64::from(rms(&output[4_096..]) / rms(&input[4_096..])).log10();
    let omega = std::f64::consts::PI / RATE;
    let ratio = (omega * hz).tan() / (omega * 1_000.0).tan();
    let numerator = if kind == Kind::HighPass {
        ratio * ratio
    } else {
        1.0
    };
    let reference = 20.0 * (numerator / (1.0 + ratio.powi(4)).sqrt()).log10();
    assert!(
        (measured - reference).abs() <= 0.5,
        "{kind:?} at {hz} Hz: {measured} dB, reference {reference} dB"
    );
}

#[kithara::test]
#[case(1)]
#[case(2)]
#[case(6)]
fn multichannel_blocks_match_the_dsp_kernel(#[case] channels: u16) {
    let settings = end_at(Kind::LowPass, 1_000.0);
    let mut effect = filter(spec(channels), settings);
    let width = usize::from(channels);
    let mut reference = StateVariableFilter::new(NonZeroUsize::new(width).expect("channels"));
    let mut planes: Vec<_> = (0..channels)
        .map(|channel| vec![f32::from(channel + 1) / 8.0; 1027])
        .collect();
    let input = interleaved(&planes);
    let (kernel, hz) = response(&settings, -1.0);
    reference
        .process(
            &mut planes,
            0..1027,
            &[hz; 1027],
            spec(channels).sample_rate,
            f64::from(settings.q()),
            kernel,
        )
        .expect("reference");
    let output = held(&mut effect, channels, &input, -1.0);
    assert_eq!(bits(&output), bits(&interleaved(&planes)));
}

#[kithara::test]
fn parameter_range_edges_keep_normalized_pcm_finite() {
    let input = [1.0, -1.0, 0.25, -0.25].repeat(1025);
    for kind in [Kind::HighPass, Kind::LowPass] {
        for hz in [Cutoff::MIN, cutoff(20_000.0)] {
            for q in [Q::MIN, Q::MAX] {
                let (edge, extreme) = match (kind, hz == Cutoff::MIN) {
                    (Kind::LowPass, true) | (Kind::HighPass, false) => (cutoff(40.0), hz),
                    (Kind::LowPass, false) | (Kind::HighPass, true) => (hz, cutoff(40.0)),
                };
                let mut effect = filter(spec(2), range(kind, edge, extreme, q));
                let side = end(kind);
                for position in [side, side * f64::EPSILON] {
                    let output = held(&mut effect, 2, &input, position);
                    assert!(output.iter().all(|sample| sample.is_finite()));
                }
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[kithara::test(native)]
fn prepared_filter_controls_process_reset_and_rate_retune_never_allocate() {
    for channels in [1, 2, 6] {
        let mut effect = filter(spec(channels), FilterSettings::default());
        let knobs = vec![-0.6; 1027];
        for (index, frames) in [0, 1, 7, 256, 1027].into_iter().enumerate() {
            let target = knob(if index % 2 == 0 { -0.6 } else { 0.4 });
            let mut planes = vec![vec![0.25; frames]; usize::from(channels)];
            assert_no_alloc::assert_no_alloc(|| {
                effect
                    .update_sample_rate(spec(channels).sample_rate)
                    .expect("same format");
                effect.configure(target);
                effect
                    .process(&mut planes, 0..frames)
                    .expect("glide output");
                effect
                    .process_program(&mut planes, 0..frames, &knobs[..frames])
                    .expect("program output");
                effect.advance(frames);
                effect.reset();
            });
            assert!(planes.iter().all(|plane| plane.len() == frames));
        }
    }
}

#[kithara::test]
fn incomplete_plane_range_refusal_advances_neither_pcm_nor_the_glide() {
    let settings = end_at(Kind::LowPass, 1_000.0);
    let mut effect = filter(spec(2), settings);
    let mut reference = filter(spec(2), settings);
    for filter in [&mut effect, &mut reference] {
        held(filter, 2, &[0.25; 14], -1.0);
        filter.configure(knob(-0.5));
        processed(filter, 2, &[0.25; 14]).expect("warm glide and history");
    }
    let mut incomplete = [vec![0.125; 7], vec![-0.25; 6]];
    let before = incomplete.clone();
    assert_eq!(
        effect.process(&mut incomplete, 0..7),
        Err(FilterError::Shape)
    );
    assert_eq!(incomplete, before);
    assert_eq!(effect.config(), reference.config());
    assert_eq!(effect.knob().to_bits(), reference.knob().to_bits());
    let actual = processed(&mut effect, 2, &[0.125; 18]).expect("continuation");
    let expected = processed(&mut reference, 2, &[0.125; 18]).expect("reference continuation");
    assert_eq!(bits(&actual), bits(&expected));
}

#[kithara::test]
fn a_program_ends_a_running_glide_at_its_last_value() {
    let mut manual = filter(spec(1), FilterSettings::default());
    let mut program = filter(spec(1), FilterSettings::default());
    for effect in [&mut manual, &mut program] {
        effect.configure(knob(-0.8));
        processed(effect, 1, &[0.25; 3]).expect("glide starts");
        assert_ne!(effect.knob(), -0.8);
        held(effect, 1, &[0.25; 3], -0.3);
        assert_eq!(effect.knob(), -0.3);
    }
    assert_eq!(
        bits(&processed(&mut manual, 1, &[0.5; 9]).expect("held after program")),
        bits(&held(&mut program, 1, &[0.5; 9], -0.3))
    );
}
