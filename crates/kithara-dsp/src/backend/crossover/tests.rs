use biquad::Hertz;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::Crossover;
use crate::filter::{FilterError, Response};

fn hz(value: f64) -> Hertz<f64> {
    Hertz::from_hz(value).expect("positive test hertz")
}

fn crossover(frequencies: &[f64], rate: f64) -> Crossover {
    Crossover::<1>::new(
        &frequencies.iter().copied().map(hz).collect::<Vec<_>>(),
        hz(rate),
    )
    .expect("valid test crossover")
}

struct ScalarSection {
    state: [f64; 2],
    gain: f64,
    response: Response,
    quiet: usize,
    reset: bool,
}

impl ScalarSection {
    fn new(response: Response, frequency: f64, rate: f64) -> Self {
        Self {
            state: [0.0; 2],
            gain: super::gain(hz(frequency), hz(rate)).expect("valid reference cutoff"),
            response,
            quiet: 0,
            reset: true,
        }
    }

    fn run(&mut self, input: f32) -> f32 {
        if self.reset && input.to_bits() == 0 {
            self.count(true);
            return input;
        }
        let damping = 1.0 / std::f64::consts::FRAC_1_SQRT_2;
        let a1 = (1.0 + self.gain * (self.gain + damping)).recip();
        let a2 = self.gain * a1;
        let a3 = self.gain * a2;
        let [s1, s2] = self.state;
        let x = f64::from(input);
        let v3 = x - s2;
        let v1 = a1 * s1 + a2 * v3;
        let v2 = s2 + a2 * s1 + a3 * v3;
        self.state = [2.0 * v1 - s1, 2.0 * v2 - s2];
        let output = (match self.response {
            Response::LowPass => v2,
            Response::HighPass => x - damping * v1 - v2,
            Response::AllPass => x - 2.0 * damping * v1,
        })
        .to_f32()
        .expect("bounded scalar oracle output");
        self.reset = false;
        self.count(
            input.abs() <= crate::consts::SILENT_PEAK && output.abs() <= crate::consts::SILENT_PEAK,
        );
        output
    }

    fn count(&mut self, quiet: bool) {
        self.quiet = if quiet {
            self.quiet
                .saturating_add(1)
                .min(crate::consts::SECTION_MEMORY)
        } else {
            0
        };
        if self.quiet == crate::consts::SECTION_MEMORY {
            self.state = [0.0; 2];
            self.reset = true;
        }
    }
}

/// Scalar TPT state and LR4/AP2 composition, independent of the block kernels.
struct Scalar {
    low: Vec<[ScalarSection; 2]>,
    high: Vec<[ScalarSection; 2]>,
    allpass: Vec<Vec<ScalarSection>>,
}

impl Scalar {
    fn new(frequencies: &[f64], rate: f64) -> Self {
        Self {
            low: frequencies
                .iter()
                .map(|&frequency| {
                    std::array::from_fn(|_| ScalarSection::new(Response::LowPass, frequency, rate))
                })
                .collect(),
            high: frequencies
                .iter()
                .map(|&frequency| {
                    std::array::from_fn(|_| ScalarSection::new(Response::HighPass, frequency, rate))
                })
                .collect(),
            allpass: (0..frequencies.len())
                .map(|band| {
                    frequencies
                        .iter()
                        .skip(band.checked_add(1).expect("next crossover index"))
                        .map(|&frequency| ScalarSection::new(Response::AllPass, frequency, rate))
                        .collect()
                })
                .collect(),
        }
    }

    fn sample(&mut self, input: f32) -> Vec<f32> {
        let mut high = input;
        let mut bands = Vec::new();
        for (lowpass, highpass) in self.low.iter_mut().zip(&mut self.high) {
            let low = lowpass[0].run(high);
            bands.push(lowpass[1].run(low));
            let next = highpass[0].run(high);
            high = highpass[1].run(next);
        }
        for (band, allpass) in bands.iter_mut().zip(&mut self.allpass) {
            for section in allpass {
                *band = section.run(*band);
            }
        }
        bands.push(high);
        bands
    }
}

fn sum(filter: &mut Crossover<1>, output: &mut [f32], gains: &[Vec<f32>]) -> usize {
    let input = output.to_vec();
    let mut bands = vec![[vec![0.0; output.len()]]; gains.len()];
    let frames = filter.split(&[input.as_slice()], &mut bands);
    let output = output.get_mut(..frames).expect("processed output prefix");
    output.fill(0.0);
    for (band, gain) in bands.iter().zip(gains) {
        crate::vector::mul_add(
            band[0].get(..frames).expect("processed band prefix"),
            gain.get(..frames).expect("processed gain prefix"),
            output,
        );
    }
    frames
}

fn input() -> Vec<f32> {
    [
        1.0,
        -0.5,
        0.25,
        -0.0,
        f32::MIN_POSITIVE,
        f32::from_bits(1),
        -f32::from_bits(1),
    ]
    .into_iter()
    .chain(std::iter::repeat_n(0.0, 8_192))
    .collect()
}

#[kithara::test]
#[case(1)]
#[case(7)]
#[case(255)]
#[case(256)]
#[case(257)]
fn every_band_matches_the_scalar_state_across_splits_reset_and_retune(#[case] chunk: usize) {
    let input = input();
    for frequencies in [
        &[][..],
        &[250.0][..],
        &[250.0, 2_500.0][..],
        &[
            100.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 6_000.0, 9_000.0, 12_000.0,
        ][..],
    ] {
        let mut actual = crossover(frequencies, 48_000.0);
        let mut summed = crossover(frequencies, 48_000.0);
        for rate in [48_000.0, 44_100.0] {
            actual.update_sample_rate(hz(rate)).expect("valid retune");
            summed.update_sample_rate(hz(rate)).expect("valid retune");
            for reset in [false, true] {
                if reset {
                    actual.reset();
                    summed.reset();
                }
                let mut scalar = Scalar::new(frequencies, rate);
                for samples in input.chunks(chunk) {
                    let count = frequencies
                        .len()
                        .checked_add(1)
                        .expect("one more band than crossovers");
                    let mut bands = vec![[vec![f32::NAN; samples.len()]]; count];
                    assert_eq!(actual.split(&[samples], &mut bands), samples.len());
                    let gains: Vec<_> = (0..count)
                        .map(|band| {
                            [0.25, -0.5, 1.25]
                                .into_iter()
                                .cycle()
                                .skip(band)
                                .take(samples.len())
                                .collect::<Vec<_>>()
                        })
                        .collect();
                    let mut output = samples.to_vec();
                    assert_eq!(sum(&mut summed, &mut output, &gains), samples.len());
                    for (frame, &sample) in samples.iter().enumerate() {
                        let mut sum = 0.0;
                        for ((band, expected), gain) in bands
                            .iter()
                            .map(|band| &band[0])
                            .zip(scalar.sample(sample))
                            .zip(&gains)
                        {
                            assert_eq!(
                                band.get(frame).expect("processed band frame").to_bits(),
                                expected.to_bits(),
                                "rate {rate} chunk {chunk} frame {frame}"
                            );
                            sum = expected.mul_add(*gain.get(frame).expect("gain frame"), sum);
                        }
                        assert_eq!(
                            output.get(frame).expect("summed frame").to_bits(),
                            sum.to_bits()
                        );
                    }
                }
            }
        }
    }
}

#[kithara::test]
fn short_planes_extra_planes_and_empty_calls_do_not_advance_unprocessed_state() {
    let frequencies = [250.0, 2_500.0];
    let mut actual = crossover(&frequencies, 48_000.0);
    let mut reference = crossover(&frequencies, 48_000.0);
    let marker = f32::from_bits(0x7fc0_1234);
    let input = [0.25, -0.5, 0.75, -1.0];
    let mut short = vec![
        [vec![marker; 6]],
        [vec![marker; 2]],
        [vec![marker; 5]],
        [vec![marker; 8]],
    ];
    assert_eq!(
        actual.split(
            &[&input[..]],
            short.get_mut(..2).expect("two required planes")
        ),
        0
    );
    assert_eq!(actual.split(&[&[][..]], &mut short), 0);
    assert_eq!(actual.split(&[&input[..]], &mut short), 2);
    let mut expected = vec![[vec![0.0; 2]]; 3];
    reference.split(&[&input[..2]], &mut expected);
    for (band, expected) in short
        .iter()
        .map(|band| &band[0])
        .zip(expected.iter().map(|band| &band[0]))
    {
        let (prefix, tail) = band.split_at_checked(2).expect("two processed frames");
        assert_eq!(
            prefix.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            expected.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
        );
        assert!(tail.iter().all(|x| x.to_bits() == marker.to_bits()));
    }
    assert!(
        short.get(3).expect("extra plane")[0]
            .iter()
            .all(|x| x.to_bits() == marker.to_bits())
    );
    short.first_mut().expect("required plane")[0].clear();
    assert_eq!(actual.split(&[&input[..]], &mut short), 0);
    let mut actual_tail = vec![[vec![0.0; 4]]; 3];
    let mut expected_tail = actual_tail.clone();
    actual.split(&[&input[..]], &mut actual_tail);
    reference.split(&[&input[..]], &mut expected_tail);
    assert_eq!(actual_tail, expected_tail);
}

#[kithara::test]
fn strict_design_rejects_invalid_rate_frequency_order_and_nyquist() {
    let zero = Hertz::from_dt(f64::INFINITY).expect("public Hz constructor can underflow");
    for rate in [zero, hz(f64::INFINITY)] {
        for frequencies in [vec![], vec![hz(250.0)]] {
            assert!(matches!(
                Crossover::<1>::new(&frequencies, rate),
                Err(FilterError::Parameters)
            ));
        }
    }
    for frequencies in [
        vec![zero],
        vec![hz(f64::INFINITY)],
        vec![hz(250.0), hz(250.0)],
        vec![hz(2_500.0), hz(250.0)],
        vec![hz(24_000.0)],
        vec![hz(24_001.0)],
    ] {
        assert!(matches!(
            Crossover::<1>::new(&frequencies, hz(48_000.0)),
            Err(FilterError::Parameters)
        ));
    }
    for value in [0.0, -1.0, f64::NEG_INFINITY, f64::NAN] {
        assert!(Hertz::<f64>::from_hz(value).is_err());
    }
    let mut empty = Crossover::<1>::new(&[], hz(48_000.0)).expect("empty valid design");
    let input = [0.25, -0.0, f32::from_bits(1)];
    let mut bands = [[vec![f32::NAN; input.len()]]];
    assert_eq!(empty.split(&[&input[..]], &mut bands), input.len());
    assert_eq!(
        bands[0][0].iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
        input.map(f32::to_bits)
    );
}

#[kithara::test]
fn butterworth_lp_dc_gain_is_unity() {
    let mut filter = crossover(&[250.0], 44_100.0);
    let input = vec![1.0; 4096];
    let mut bands = vec![[vec![0.0; input.len()]]; 2];
    assert_eq!(filter.split(&[&input[..]], &mut bands), input.len());
    let dc_gain = *bands.first().expect("low band")[0]
        .last()
        .expect("nonempty DC response");
    assert!((dc_gain - 1.0).abs() < 0.001);
}

#[kithara::test]
fn coefficient_factory_errors_and_nonfinite_results_are_not_identity() {
    assert!(matches!(
        super::gain(hz(24_000.0), hz(48_000.0)),
        Err(FilterError::Parameters)
    ));
    assert!(matches!(
        super::gain(hz(f64::INFINITY), hz(48_000.0)),
        Err(FilterError::Parameters)
    ));
}

#[kithara::test]
fn a_refused_retune_preserves_every_sections_history() {
    let mut actual = crossover(&[250.0, 2_500.0], 48_000.0);
    let mut reference = crossover(&[250.0, 2_500.0], 48_000.0);
    let mut warm = vec![[vec![0.0; 257]]; 3];
    let input = input();
    let prefix = input.get(..257).expect("warmup prefix");
    actual.split(&[prefix], &mut warm);
    reference.split(&[prefix], &mut warm);
    assert!(matches!(
        actual.update_sample_rate(hz(1_000.0)),
        Err(FilterError::Parameters)
    ));
    let tail = [0.25; 257];
    let mut output = vec![[vec![0.0; tail.len()]]; 3];
    let mut expected = output.clone();
    actual.split(&[&tail[..]], &mut output);
    reference.split(&[&tail[..]], &mut expected);
    for (actual, expected) in output
        .iter()
        .flatten()
        .flatten()
        .zip(expected.iter().flatten().flatten())
    {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}

#[kithara::test]
fn stereo_quiet_retirement_keeps_each_mono_history() {
    let frequencies = [hz(250.0), hz(2_500.0)];
    let mut stereo = Crossover::<2>::new(&frequencies, hz(48_000.0)).expect("stereo design");
    let mut mono = std::array::from_fn::<_, 2, _>(|_| crossover(&[250.0, 2_500.0], 48_000.0));
    for rate in [48_000.0, 44_100.0] {
        stereo.update_sample_rate(hz(rate)).expect("stereo retune");
        for mono in &mut mono {
            mono.update_sample_rate(hz(rate)).expect("mono retune");
        }
        for frames in [1, 7, 255, 256, 257, 8192] {
            let left: Vec<_> = input().into_iter().cycle().take(frames).collect();
            let right: Vec<_> = [0.0, -0.0, 1e-10, f32::from_bits(1)]
                .into_iter()
                .cycle()
                .take(frames)
                .collect();
            let mut actual = vec![[vec![0.0; frames], vec![0.0; frames]]; 3];
            stereo.split(&[left.as_slice(), right.as_slice()], &mut actual);
            for (channel, (mono, input)) in mono.iter_mut().zip([left, right]).enumerate() {
                let mut expected = vec![[vec![0.0; frames]]; 3];
                mono.split(&[input.as_slice()], &mut expected);
                for (band, expected) in actual.iter().zip(&expected) {
                    for (actual, expected) in band
                        .get(channel)
                        .expect("stereo channel")
                        .iter()
                        .zip(&expected[0])
                    {
                        assert_eq!(
                            actual.to_bits(),
                            expected.to_bits(),
                            "channel{channel} frames{frames} rate{rate}"
                        );
                    }
                }
            }
        }
    }
}
