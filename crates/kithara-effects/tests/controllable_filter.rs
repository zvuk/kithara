#![forbid(unsafe_code)]

use std::num::{NonZeroU32, NonZeroUsize};

use kithara_config::ConfigOwner;
use kithara_dsp::filter::{Biquad, Coefficients, FilterError, Hertz, Type};
use kithara_effects::filter::{
    Cutoff, FilterConfig, FilterEffect, FilterKnob, FilterMotion, FilterSettings, Kind,
    PrepareError, Q,
};
use kithara_signal::AudioSpec;
use kithara_test_fixtures::{
    signal::rms,
    unit_fixtures::{eq_sine_40, eq_sine_1000, eq_sine_10000},
};
use kithara_test_utils::{
    bufpool::{Pools, TestPools, pools, pools_with_budget},
    kithara,
};

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

/// Settings whose end of the `kind` side sits exactly at `hz`, with `q`.
fn end_at(kind: Kind, hz: f64, q: Q) -> FilterSettings {
    range(kind, if kind == Kind::LowPass { Cutoff::MAX } else { Cutoff::MIN }, cutoff(hz), q)
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

/// The knob end of the `kind` side.
fn end(kind: Kind) -> FilterKnob {
    match kind {
        Kind::LowPass => FilterKnob::MIN,
        Kind::HighPass => FilterKnob::MAX,
    }
}

/// A filter running at the end of the `kind` side, with its cutoff at `hz`.
fn filter_at(pools: &Pools, spec: AudioSpec, kind: Kind, hz: f64) -> FilterEffect {
    let config = FilterConfig::<TestPools>::from((pools.clone(), end_at(kind, hz, Q::default())));
    let mut filter = FilterEffect::new(spec, config).expect("prepared filter");
    filter.set_explicit(end(kind)).expect("stable end response");
    filter
}

fn design(kind: Type<f64>, cutoff: f64) -> Coefficients<f64> {
    Coefficients::from_params(
        kind,
        Hertz::from_hz(44_100.0).expect("reference rate"),
        Hertz::from_hz(cutoff).expect("reference cutoff"),
        std::f64::consts::FRAC_1_SQRT_2,
    )
    .expect("reference coefficients")
}

fn low_pass_reference(channels: NonZeroUsize) -> Biquad {
    let mut filter = Biquad::new(channels, NonZeroUsize::MIN).expect("reference filter");
    let settings = end_at(Kind::LowPass, 1_000.0, Q::default());
    let (_, hz) = settings
        .response(f64::from(FilterKnob::MIN))
        .expect("low-pass end");
    filter
        .retune(0, design(Type::LowPass, hz))
        .expect("reference section");
    filter
}

fn processed(
    filter: &mut FilterEffect,
    channels: u16,
    input: &[f32],
) -> Result<Vec<f32>, FilterError> {
    let width = usize::from(channels);
    assert!(width > 0);
    assert_eq!(input.len() % width, 0, "fixture contains whole frames");
    let frames = input.len() / width;
    let mut planes: Vec<Vec<_>> = (0..width)
        .map(|channel| input.iter().skip(channel).step_by(width).copied().collect())
        .collect();
    filter.process(&mut planes, 0..frames)?;
    Ok((0..frames)
        .flat_map(|frame| planes.iter().map(move |plane| plane[frame]))
        .collect())
}

fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|sample| sample.to_bits()).collect()
}

#[kithara::test]
fn manual_knob_follows_the_f64_smoothing_law_across_odd_blocks(eq_sine_1000: Vec<f32>) {
    let pools = pools();
    let settings = FilterSettings::default();
    let config = || FilterConfig::<TestPools>::from((pools.clone(), settings));
    let mut whole = FilterEffect::new(spec(1), config()).expect("prepared filter");
    let mut split = FilterEffect::new(spec(1), config()).expect("prepared filter");
    let (from, to) = (-0.3, -0.7);
    let cutoff_at = |value: f64| settings.response(value).expect("low-pass side").1;
    let mut reference = Biquad::new(NonZeroUsize::MIN, NonZeroUsize::MIN).expect("reference");
    reference
        .retune(0, design(Type::LowPass, cutoff_at(from)))
        .expect("reference start");
    let mut warmup = eq_sine_1000[..4096].to_vec();
    reference
        .process(&mut [warmup.as_mut_slice()], 0..4096)
        .expect("reference warmup");
    for filter in [&mut whole, &mut split] {
        filter.set_explicit(knob(from)).expect("start position");
        processed(filter, 1, &eq_sine_1000[..4096]).expect("warmup");
        filter
            .configure(knob(to))
            .expect("manual glide is admitted");
    }
    let input = &eq_sine_1000[4096..12_288];
    let mut expected = input.to_vec();
    let smoothing = kithara_dsp::param::SmootherConfig::default();
    let ratio = f64::from(smoothing.settle_ratio);
    let b1 = ratio.powf((f64::from(smoothing.smooth_seconds) * 44_100.0).recip());
    let mut value = from;
    for frame in 0..expected.len() {
        // The effect retunes only while the knob moves: re-applying equal
        // coefficients is not bit-neutral on every backend.
        if value != to {
            value = to * (1.0 - b1) + value * b1;
            if (value - to).abs() < (to - from).abs() * ratio {
                value = to;
            }
            reference
                .retune(0, design(Type::LowPass, cutoff_at(value)))
                .expect("stable reference section");
        }
        reference
            .process(&mut [expected.as_mut_slice()], frame..frame + 1)
            .expect("reference frame");
    }
    let actual = processed(&mut whole, 1, input).expect("whole block");
    assert_eq!(bits(&actual), bits(&expected));
    let mut partitioned = Vec::with_capacity(input.len());
    for block in input.chunks(7) {
        partitioned.extend_from_slice(&processed(&mut split, 1, block).expect("split block"));
    }
    assert_eq!(bits(&partitioned), bits(&expected));
    assert_eq!(value, to);
    assert_eq!(whole.knob(), to);
}

#[kithara::test]
fn the_center_passes_the_signal_bit_for_bit_and_holds_no_history() {
    let pools = pools();
    let mut filter = FilterEffect::new(
        spec(2),
        FilterConfig::<TestPools>::from((pools, FilterSettings::default())),
    )
    .expect("prepared filter");
    let input = [0.25, -0.5, 1e-30, f32::MIN_POSITIVE, -0.0, 0.75, 1.0, -1.0];
    let output = processed(&mut filter, 2, &input).expect("center");
    assert_eq!(bits(&output), bits(&input));
    assert!(filter.tail_exhausted());
}

#[kithara::test]
fn a_move_through_the_center_restarts_the_other_side_from_reset_state() {
    let pools = pools();
    let config = || FilterConfig::<TestPools>::from((pools.clone(), FilterSettings::default()));
    let mut swept = FilterEffect::new(spec(1), config()).expect("swept filter");
    let mut fresh = FilterEffect::new(spec(1), config()).expect("fresh filter");
    let input = [0.25, -0.5, 0.75, 0.125].repeat(64);
    swept.set_explicit(knob(-0.4)).expect("low-pass side");
    let low = processed(&mut swept, 1, &input).expect("low-pass history");
    assert_ne!(bits(&low), bits(&input));
    assert!(!swept.tail_exhausted());
    swept.set_explicit(knob(0.4)).expect("through the center");
    fresh.set_explicit(knob(0.4)).expect("high-pass side");
    assert_eq!(
        bits(&processed(&mut swept, 1, &input).expect("after the move")),
        bits(&processed(&mut fresh, 1, &input).expect("from reset state"))
    );
    swept.set_explicit(FilterKnob::default()).expect("center");
    assert!(swept.tail_exhausted());
    assert_eq!(
        bits(&processed(&mut swept, 1, &input).expect("center again")),
        bits(&input)
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
    let pools = pools();
    assert!(matches!(
        FilterEffect::new(
            spec(0),
            FilterConfig::<TestPools>::from((pools, FilterSettings::default()))
        ),
        Err(PrepareError::Shape)
    ));
}

#[kithara::test]
fn filter_is_a_causal_planar_consumer_with_independent_channel_history() {
    let pools = pools();
    let mut filter = filter_at(&pools, spec(2), Kind::HighPass, 1_000.0);
    let input = [0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let output = processed(&mut filter, 2, &input).expect("causal output");
    assert_eq!(output.len(), input.len());
    assert_ne!(&output[..], &input);
    assert!(
        output
            .iter()
            .skip(1)
            .step_by(2)
            .all(|sample| sample.to_bits() == 0)
    );
    filter.reset().expect("reset");
    assert!(filter.tail_exhausted());
    assert!(
        processed(&mut filter, 2, &[0.0; 8])
            .expect("reset silence")
            .iter()
            .all(|sample| sample.to_bits() == 0)
    );
}

#[kithara::test]
fn filter_keeps_history_across_odd_blocks_and_reset_clears_it() {
    let pools = pools();
    let mut whole = filter_at(&pools, spec(1), Kind::LowPass, 1_000.0);
    let mut split = filter_at(&pools, spec(1), Kind::LowPass, 1_000.0);
    let mut input = vec![0.0; 257];
    input[0] = 0.25;
    input[97] = -0.125;
    let expected = processed(&mut whole, 1, &input).expect("whole chunk");
    let mut actual = Vec::new();
    for block in input.chunks(7) {
        actual.extend_from_slice(&processed(&mut split, 1, block).expect("split chunk"));
    }
    assert_eq!(actual, &expected[..]);
    split.reset().expect("reset");
    let silence = processed(&mut split, 1, &[0.0; 16]).expect("silence after reset");
    assert!(silence.iter().all(|sample| sample.to_bits() == 0));
    let replay = processed(&mut split, 1, &input).expect("same response after reset");
    assert_eq!(&replay[..], &expected[..]);
}

/// The mono 44.1 kHz fixture pins the Butterworth cutoff within 0.5 dB after
/// discarding 4096 samples of startup history.
#[kithara::test]
#[case(Kind::HighPass)]
#[case(Kind::LowPass)]
fn filter_matches_declared_corner_response(eq_sine_1000: Vec<f32>, #[case] kind: Kind) {
    let pools = pools();
    let mut filter = filter_at(&pools, spec(1), kind, 1_000.0);
    let output = processed(&mut filter, 1, &eq_sine_1000).expect("filtered tone");
    let measured_db = 20.0 * f64::from(rms(&output[4_096..]) / rms(&eq_sine_1000[4_096..])).log10();
    assert!(
        (measured_db + 3.010_299_956_64).abs() <= 0.5,
        "{kind:?}: {measured_db} dB"
    );
}

/// The DC group delay of a second-order Butterworth LP is 1/(2 Q tan(pi fc/fs)).
#[kithara::test]
fn low_pass_impulse_has_the_declared_causal_delay() {
    let pools = pools();
    let hz = 200.0;
    let mut filter = filter_at(&pools, spec(1), Kind::LowPass, hz);
    let mut input = vec![0.0; 8192];
    let origin = 2048;
    *input.get_mut(origin).expect("impulse origin") = 0.25;
    let output = processed(&mut filter, 1, &input).expect("impulse");
    assert!(
        output
            .get(..origin)
            .expect("impulse prefix")
            .iter()
            .all(|sample| *sample == 0.0)
    );
    let (sum, moment) = output
        .get(origin..)
        .expect("impulse response")
        .iter()
        .enumerate()
        .fold((0.0_f64, 0.0_f64), |(sum, moment), (frame, sample)| {
            let frame = f64::from(u32::try_from(frame).expect("bounded impulse"));
            let sample = f64::from(*sample);
            (sum + sample, moment + frame * sample)
        });
    assert!((20.0 * (sum / 0.25).abs().log10()).abs() <= 0.5);
    let delay = moment / sum;
    let reference = std::f64::consts::FRAC_1_SQRT_2 / (std::f64::consts::PI * hz / 44_100.0).tan();
    assert!(
        (delay - reference).abs() <= 0.1,
        "DC group delay: {delay} frames, causal reference {reference} frames"
    );
}

#[kithara::test]
fn refused_rate_and_plane_geometry_leave_format_and_history_untouched() {
    let pools = pools();
    let mut filter = filter_at(&pools, spec(1), Kind::HighPass, 20_000.0);
    let mut reference = filter_at(&pools, spec(1), Kind::HighPass, 20_000.0);
    for effect in [&mut filter, &mut reference] {
        assert!(
            processed(effect, 1, &[0.25, 0.0, 0.0])
                .expect("warm history")
                .iter()
                .any(|sample| *sample != 0.0)
        );
    }
    let refused = AudioSpec::new(1, NonZeroU32::new(32_000).expect("changed rate"));
    let mut invalid_planes: [&mut [f32]; 0] = [];
    assert_eq!(
        filter.process(&mut invalid_planes, 0..0),
        Err(FilterError::Shape)
    );
    assert!(matches!(
        filter.update_sample_rate(refused.sample_rate),
        Err(PrepareError::Filter(FilterError::Parameters))
    ));
    assert!(!filter.accepts_spec(refused));
    assert!(filter.accepts_spec(spec(1)));
    let expected = processed(&mut reference, 1, &[0.0; 3]).expect("reference");
    let actual = processed(&mut filter, 1, &[0.0; 3]).expect("accepted input");
    assert!(expected.iter().any(|sample| *sample != 0.0));
    assert_eq!(actual, expected);
}

#[kithara::test]
fn refused_cold_replacement_preserves_resident_filter_and_history() {
    let scratch = pools_with_budget(1024);
    let pcm = pools();
    let mut filter = filter_at(&scratch, spec(1), Kind::LowPass, 1_000.0);
    let mut reference = filter_at(&pcm, spec(1), Kind::LowPass, 1_000.0);
    for effect in [&mut filter, &mut reference] {
        processed(effect, 1, &[0.25; 7]).expect("warm history");
    }
    assert!(matches!(
        FilterEffect::new(
            spec(2),
            FilterConfig::<TestPools>::from((scratch.clone(), FilterSettings::default()))
        ),
        Err(PrepareError::Pool(_))
    ));
    assert!(filter.accepts_spec(spec(1)));
    assert!(!filter.accepts_spec(spec(2)));
    let actual = processed(&mut filter, 1, &[0.0; 7]).expect("old format");
    let expected = processed(&mut reference, 1, &[0.0; 7]).expect("reference");
    assert!(expected.iter().any(|sample| *sample != 0.0));
    assert_eq!(actual, expected);
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
    let pools = pools();
    let mut filter = filter_at(&pools, spec(1), kind, 1_000.0);
    let output = processed(&mut filter, 1, &input).expect("filtered tone");
    let measured = 20.0 * f64::from(rms(&output[4_096..]) / rms(&input[4_096..])).log10();
    let omega = std::f64::consts::PI / 44_100.0;
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
#[case(7)]
fn filter_matches_dsp_quiet_history_within_a_block(#[case] block_frames: usize) {
    let pools = pools();
    let mut filter = filter_at(&pools, spec(1), Kind::LowPass, 1_000.0);
    let mut reference = low_pass_reference(NonZeroUsize::MIN);
    let input = [1e-12, -1e-12, 0.0, 0.0, 0.25, 0.0, 0.0];
    for block in input.chunks(block_frames) {
        let mut expected = block.to_vec();
        reference
            .process(&mut [expected.as_mut_slice()], 0..block.len())
            .expect("reference block");
        let output = processed(&mut filter, 1, block).expect("block");
        assert_eq!(&output[..], &expected);
    }
}

/// One reset setup and one allocation isolate call length and plane offset.
#[kithara::test]
#[case::short_same_start(7, 0)]
#[case::full_shift_one(256, 1)]
#[case::full_shift_two(256, 2)]
#[case::full_shift_three(256, 3)]
fn biquad_impulse_prefix_is_independent_of_length_and_offset(
    #[case] frames: usize,
    #[case] offset: usize,
) {
    let mut filter = low_pass_reference(NonZeroUsize::MIN);
    let mut plane = vec![0.0; 259];
    plane[0] = 0.25;
    filter
        .process(&mut [plane.as_mut_slice()], 0..256)
        .expect("reference block");
    let expected: Vec<_> = plane[..7].iter().map(|sample| sample.to_bits()).collect();
    filter.reset();
    plane.fill(0.0);
    plane[offset] = 0.25;
    filter
        .process(&mut [plane.as_mut_slice()], offset..offset + frames)
        .expect("same allocation after reset");
    let actual: Vec<_> = plane[offset..offset + 7]
        .iter()
        .map(|sample| sample.to_bits())
        .collect();
    assert_eq!(actual, expected);
}

#[kithara::test]
fn parameter_range_edges_keep_normalized_pcm_finite() {
    let pools = pools();
    let input = [1.0, -1.0, 0.25, -0.25].repeat(1025);
    for kind in [Kind::HighPass, Kind::LowPass] {
        for hz in [Cutoff::MIN, Cutoff::MAX] {
            for q in [Q::MIN, Q::MAX] {
                let (edge, extreme) = match (kind, hz == Cutoff::MIN) {
                    (Kind::LowPass, true) => (cutoff(40.0), hz),
                    (Kind::LowPass, false) => (hz, cutoff(40.0)),
                    (Kind::HighPass, true) => (hz, cutoff(40.0)),
                    (Kind::HighPass, false) => (cutoff(40.0), hz),
                };
                let settings = range(kind, edge, extreme, q);
                let mut filter = FilterEffect::new(
                    spec(2),
                    FilterConfig::<TestPools>::from((pools.clone(), settings)),
                )
                .expect("prepared filter");
                for position in [-1.0, -f64::EPSILON, f64::EPSILON, 1.0] {
                    if filter.set_explicit(knob(position)).is_ok() {
                        let output = processed(&mut filter, 2, &input).expect("output");
                        assert!(output.iter().all(|sample| sample.is_finite()));
                    }
                }
            }
        }
    }
}

#[kithara::test]
#[case(1)]
#[case(2)]
#[case(6)]
fn pooled_blocks_match_multichannel_dsp(#[case] channels: u16) {
    let pools = pools();
    let mut filter = filter_at(&pools, spec(channels), Kind::LowPass, 1_000.0);
    let width = usize::from(channels);
    let mut reference = low_pass_reference(NonZeroUsize::new(width).expect("channels"));
    let mut planes: Vec<_> = (0..channels)
        .map(|channel| vec![f32::from(channel + 1) / 8.0; 1027])
        .collect();
    let input: Vec<_> = (0..1027)
        .flat_map(|frame| planes.iter().map(move |plane| plane[frame]))
        .collect();
    for start in (0..1027).step_by(256) {
        reference
            .process(&mut planes, start..(start + 256).min(1027))
            .expect("reference block");
    }
    let output = processed(&mut filter, channels, &input).expect("output");
    for (frame, samples) in output.chunks_exact(width).enumerate() {
        for (channel, sample) in samples.iter().enumerate() {
            assert_eq!(sample.to_bits(), planes[channel][frame].to_bits());
        }
    }
}

#[kithara::test]
fn filter_preparation_reports_insufficient_scratch_budget() {
    let pools = pools_with_budget(512);
    assert!(matches!(
        FilterEffect::new(
            spec(2),
            FilterConfig::<TestPools>::from((pools, FilterSettings::default()))
        ),
        Err(PrepareError::Pool(_))
    ));
}

#[cfg(not(target_arch = "wasm32"))]
#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[kithara::test(native)]
fn prepared_filter_controls_process_reset_and_rate_retune_never_allocate() {
    let pools = pools();
    for channels in [1, 2, 6] {
        let mut filter = FilterEffect::new(
            spec(channels),
            FilterConfig::<TestPools>::from((pools.clone(), FilterSettings::default())),
        )
        .expect("prepared filter");
        for (index, frames) in [0, 1, 7, 256, 1027].into_iter().enumerate() {
            let target = knob(if index.is_multiple_of(2) { -0.6 } else { 0.4 });
            let mut planes = vec![vec![0.25; frames]; usize::from(channels)];
            let allocated = pools.stats().allocated_bytes;
            assert_no_alloc::assert_no_alloc(|| {
                filter
                    .update_sample_rate(spec(channels).sample_rate)
                    .expect("same format");
                filter.configure(target).expect("bounded manual glide");
                filter
                    .process(&mut planes, 0..frames)
                    .expect("immediate output");
                filter.set_explicit(target).expect("program point");
                filter
                    .process(&mut planes, 0..frames)
                    .expect("program output");
                filter.reset().expect("reset");
                assert!(filter.tail_exhausted());
            });
            assert!(planes.iter().all(|plane| plane.len() == frames));
            assert_eq!(pools.stats().allocated_bytes, allocated);
        }
    }
}

#[kithara::test]
fn incomplete_plane_range_refusal_advances_neither_pcm_nor_manual_history() {
    let pools = pools();
    let mut filter = filter_at(&pools, spec(2), Kind::LowPass, 1_000.0);
    let mut reference = filter_at(&pools, spec(2), Kind::LowPass, 1_000.0);
    for effect in [&mut filter, &mut reference] {
        effect.configure(knob(0.5)).expect("manual glide");
        processed(effect, 2, &[0.25; 14]).expect("warm glide and history");
    }
    let mut incomplete = [vec![0.125; 7], vec![-0.25; 6]];
    let before = incomplete.clone();
    assert_eq!(
        filter.process(&mut incomplete, 0..7),
        Err(FilterError::Shape)
    );
    assert_eq!(incomplete, before);
    assert_eq!(filter.config(), reference.config());
    assert_eq!(filter.knob().to_bits(), reference.knob().to_bits());
    let actual = processed(&mut filter, 2, &[0.125; 18]).expect("continuation");
    let expected = processed(&mut reference, 2, &[0.125; 18]).expect("reference continuation");
    assert_eq!(bits(&actual), bits(&expected));
}

#[kithara::test]
fn manual_refusal_keeps_the_glide_and_filter_history() {
    let pools = pools();
    let low = AudioSpec::new(1, NonZeroU32::new(32_000).expect("rate"));
    let settings = end_at(Kind::HighPass, 20_000.0, Q::default());
    let mut filters: Vec<_> = (0..2)
        .map(|_| {
            FilterEffect::new(
                low,
                FilterConfig::<TestPools>::from((pools.clone(), settings)),
            )
            .expect("prepared")
        })
        .collect();
    for filter in &mut filters {
        filter.configure(knob(0.3)).expect("admitted glide");
        processed(filter, 1, &[0.25; 7]).expect("warm glide and history");
    }
    assert!(filters[0].configure(FilterKnob::MAX).is_err());
    let output: Vec<_> = filters
        .iter_mut()
        .map(|filter| processed(filter, 1, &[0.125; 257]).expect("old admitted state"))
        .collect();
    assert_eq!(output[0], output[1]);
}

#[kithara::test]
fn admitted_switching_keeps_normalized_pcm_finite_or_refuses_before_mutation() {
    let pools = pools();
    let settings = FilterSettings::builder()
        .low_edge(cutoff(11_025.0))
        .low_extreme(Cutoff::MIN)
        .q(Q::MAX)
        .build()
        .expect("stable end settings");
    let config = FilterConfig::<TestPools>::from((pools, settings));
    let mut filter = FilterEffect::new(spec(1), config.clone()).expect("filter");
    let mut reference = FilterEffect::new(spec(1), config).expect("unchanged reference");
    let near_center = -f64::EPSILON;
    for effect in [&mut filter, &mut reference] {
        effect.set_explicit(FilterKnob::MIN).expect("low end");
        processed(effect, 1, &[1.0]).expect("retained history");
    }
    let rate = spec(1).sample_rate;
    let travel = settings
        .travel(44_100.0, -1.0, near_center)
        .expect("cutoff travel");
    let motion = FilterMotion::new(rate, -1.0..=near_center, -1.0, 63.0 * travel, 552 * 32, 63)
        .expect("exact HOLD path");
    if let Err(error) = filter.admit_trajectory(rate, motion) {
        assert_eq!(error, FilterError::Parameters);
        assert_eq!(
            processed(&mut filter, 1, &[1.0; 7]).expect("old continuation"),
            processed(&mut reference, 1, &[1.0; 7]).expect("unchanged continuation")
        );
        return;
    }
    for frame in 0..552 * 32 {
        let value = if frame % 552 == 551 {
            near_center
        } else {
            -1.0
        };
        filter
            .set_explicit(knob(value))
            .expect("admitted trajectory point");
        let mut plane = [1.0];
        filter
            .process(&mut [plane.as_mut_slice()], 0..1)
            .expect("normalized frame");
        assert!(
            plane[0].is_finite(),
            "nonfinite accepted PCM at frame{frame}"
        );
    }
}

#[kithara::test]
fn ordinary_sweep_admission_preserves_the_sounding_filter() {
    let pools = pools();
    let settings = FilterSettings::builder()
        .low_edge(cutoff(800.0))
        .low_extreme(cutoff(200.0))
        .q(Q::MAX)
        .build()
        .expect("settings");
    let config = FilterConfig::<TestPools>::from((pools, settings));
    let mut filter = FilterEffect::new(spec(1), config.clone()).expect("filter");
    let mut reference = FilterEffect::new(spec(1), config).expect("reference");
    for effect in [&mut filter, &mut reference] {
        effect.set_explicit(FilterKnob::MIN).expect("200 Hz");
        processed(effect, 1, &[1.0; 7]).expect("retained history");
    }
    let rate = spec(1).sample_rate;
    let near_center = -f64::EPSILON;
    let motion = FilterMotion::new(
        rate,
        -1.0..=near_center,
        -1.0,
        settings
            .travel(44_100.0, -1.0, near_center)
            .expect("cutoff travel"),
        44_101,
        44_100,
    )
    .expect("one-second logarithmic sweep");
    let before = *filter.config();
    filter
        .admit_trajectory(rate, motion)
        .expect("ordinary sweep");
    assert_eq!(*filter.config(), before);
    assert_eq!(
        processed(&mut filter, 1, &[1.0; 7]).expect("admission continuation"),
        processed(&mut reference, 1, &[1.0; 7]).expect("unchanged continuation")
    );
}

/// A minute-long sweep is an ordinary program: the filter forgets old
/// rounding error faster than a slow sweep adds it, so length alone must not
/// refuse it.
#[kithara::test]
fn a_sixty_second_sweep_from_200_to_8000_hz_is_admitted() {
    let rate = NonZeroU32::new(48_000).expect("rate");
    let settings = FilterSettings::default();
    let position = |hz: f64| {
        let (edge, extreme) = (30.0, 10_000.0);
        (hz / edge).ln() / (extreme / edge).ln()
    };
    let (from, to) = (position(200.0), position(8_000.0));
    for (value, hz) in [(from, 200.0), (to, 8_000.0)] {
        let (kind, actual) = settings.response(value).expect("high-pass side");
        assert_eq!(kind, Kind::HighPass);
        assert!((actual - hz).abs() < 1e-6 * hz, "{actual} Hz for {hz} Hz");
    }
    let mut filter = FilterEffect::new(
        AudioSpec::new(2, rate),
        FilterConfig::<TestPools>::from((pools(), settings)),
    )
    .expect("filter");
    filter.set_explicit(knob(from)).expect("sweep start");
    processed(&mut filter, 2, &[1.0, -1.0].repeat(64)).expect("retained history");
    let frames = 60 * u64::from(rate.get());
    let motion = FilterMotion::new(
        rate,
        from..=to,
        from,
        settings
            .travel(f64::from(rate.get()), from, to)
            .expect("travel"),
        frames + 1,
        frames,
    )
    .expect("sweep summary");
    assert_eq!(filter.admit_trajectory(rate, motion), Ok(()));
}

#[kithara::test]
fn equal_knob_extrema_do_not_erase_trajectory_travel() {
    let pools = pools();
    let settings = FilterSettings::builder()
        .low_edge(cutoff(11_025.0))
        .low_extreme(Cutoff::MIN)
        .q(Q::MAX)
        .build()
        .expect("settings");
    let mut filter = FilterEffect::new(spec(1), FilterConfig::<TestPools>::from((pools, settings)))
        .expect("filter");
    filter.set_explicit(FilterKnob::MIN).expect("low end");
    let rate = spec(1).sample_rate;
    let near_center = -f64::EPSILON;
    let travel = settings
        .travel(44_100.0, -1.0, near_center)
        .expect("cutoff travel");
    let once =
        FilterMotion::new(rate, -1.0..=near_center, -1.0, travel, 552 * 32, 1).expect("one jump");
    assert!(filter.admit_trajectory(rate, once).is_ok());
    let repeated = FilterMotion::new(rate, -1.0..=near_center, -1.0, 63.0 * travel, 552 * 32, 63)
        .expect("repeated jumps");
    assert_eq!(
        filter.admit_trajectory(rate, repeated),
        Err(FilterError::Parameters)
    );
}
