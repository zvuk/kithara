use biquad::{Biquad, Coefficients, DirectForm1, Hertz, Type};
use kithara_test_utils::kithara;

use super::Crossover;
use crate::filter::FilterError;

fn hz(value: f32) -> Hertz<f32> {
    Hertz::from_hz(value).expect("positive test hertz")
}

fn crossover(frequencies: &[f32], rate: f32) -> Crossover {
    Crossover::new(
        &frequencies.iter().copied().map(hz).collect::<Vec<_>>(),
        hz(rate),
    )
    .expect("valid test crossover")
}

struct ScalarSection(DirectForm1<f32>);

impl ScalarSection {
    fn new(kind: Type<f32>, frequency: f32, rate: f32) -> Self {
        Self(DirectForm1::new(
            Coefficients::from_normalized_params(
                kind,
                2.0 * frequency / rate,
                std::f32::consts::FRAC_1_SQRT_2,
            )
            .unwrap_or(Coefficients {
                a1: 0.0,
                a2: 0.0,
                b0: 1.0,
                b1: 0.0,
                b2: 0.0,
            }),
        ))
    }

    fn run(&mut self, input: f32) -> f32 {
        let output = self.0.run(input);
        if output.is_subnormal() && input.abs() < f32::MIN_POSITIVE {
            self.0.reset_state();
            0.0
        } else {
            output
        }
    }
}

/// The original frame-first crossover, independent of the block implementation.
struct Scalar {
    low: Vec<[ScalarSection; 2]>,
    high: Vec<[ScalarSection; 2]>,
    allpass: Vec<Vec<ScalarSection>>,
}

impl Scalar {
    fn new(frequencies: &[f32], rate: f32) -> Self {
        Self {
            low: frequencies
                .iter()
                .map(|&frequency| {
                    std::array::from_fn(|_| ScalarSection::new(Type::LowPass, frequency, rate))
                })
                .collect(),
            high: frequencies
                .iter()
                .map(|&frequency| {
                    std::array::from_fn(|_| ScalarSection::new(Type::HighPass, frequency, rate))
                })
                .collect(),
            allpass: (0..frequencies.len())
                .map(|band| {
                    frequencies
                        .iter()
                        .skip(band.checked_add(1).expect("next crossover index"))
                        .map(|&frequency| ScalarSection::new(Type::AllPass, frequency, rate))
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
                    let mut bands = vec![vec![f32::NAN; samples.len()]; count];
                    assert_eq!(actual.split(samples, &mut bands), samples.len());
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
                    assert_eq!(summed.sum(&mut output, &gains), samples.len());
                    for (frame, &sample) in samples.iter().enumerate() {
                        let mut sum = 0.0;
                        for ((band, expected), gain) in
                            bands.iter().zip(scalar.sample(sample)).zip(&gains)
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
        vec![marker; 6],
        vec![marker; 2],
        vec![marker; 5],
        vec![marker; 8],
    ];
    assert_eq!(
        actual.split(&input, short.get_mut(..2).expect("two required planes")),
        0
    );
    assert_eq!(actual.split(&[], &mut short), 0);
    assert_eq!(actual.split(&input, &mut short), 2);
    let mut expected = vec![vec![0.0; 2]; 3];
    reference.split(&input[..2], &mut expected);
    for (band, expected) in short.iter().zip(&expected) {
        let (prefix, tail) = band.split_at_checked(2).expect("two processed frames");
        assert_eq!(
            prefix.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            expected.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
        );
        assert!(tail.iter().all(|x| x.to_bits() == marker.to_bits()));
    }
    assert!(
        short
            .get(3)
            .expect("extra plane")
            .iter()
            .all(|x| x.to_bits() == marker.to_bits())
    );
    short.first_mut().expect("required plane").clear();
    assert_eq!(actual.split(&input, &mut short), 0);
    let mut actual_tail = vec![vec![0.0; 4]; 3];
    let mut expected_tail = actual_tail.clone();
    actual.split(&input, &mut actual_tail);
    reference.split(&input, &mut expected_tail);
    assert_eq!(actual_tail, expected_tail);
}

#[kithara::test]
fn strict_design_rejects_invalid_rate_frequency_order_and_nyquist() {
    let zero = Hertz::from_dt(f32::INFINITY).expect("public Hz constructor can underflow");
    for rate in [zero, hz(f32::INFINITY)] {
        for frequencies in [vec![], vec![hz(250.0)]] {
            assert!(matches!(
                Crossover::new(&frequencies, rate),
                Err(FilterError::Parameters)
            ));
        }
    }
    for frequencies in [
        vec![zero],
        vec![hz(f32::INFINITY)],
        vec![hz(250.0), hz(250.0)],
        vec![hz(2_500.0), hz(250.0)],
        vec![hz(24_000.0)],
        vec![hz(24_001.0)],
    ] {
        assert!(matches!(
            Crossover::new(&frequencies, hz(48_000.0)),
            Err(FilterError::Parameters)
        ));
    }
    for value in [0.0, -1.0, f32::NEG_INFINITY, f32::NAN] {
        assert!(Hertz::<f32>::from_hz(value).is_err());
    }
    let mut empty = Crossover::new(&[], hz(48_000.0)).expect("empty valid design");
    let input = [0.25, -0.0, f32::from_bits(1)];
    let mut bands = [vec![f32::NAN; input.len()]];
    assert_eq!(empty.split(&input, &mut bands), input.len());
    assert_eq!(
        bands[0].iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
        input.map(f32::to_bits)
    );
}

#[kithara::test]
fn butterworth_lp_dc_gain_is_unity() {
    let coeffs = super::coefficients(Type::LowPass, hz(250.0), hz(44100.0)).expect("valid design");
    let dc_gain = (coeffs.b0 + coeffs.b1 + coeffs.b2) / (1.0 + coeffs.a1 + coeffs.a2);
    assert!((dc_gain - 1.0).abs() < 0.001);
}

#[kithara::test]
fn coefficient_factory_errors_and_nonfinite_results_are_not_identity() {
    assert!(matches!(
        super::coefficients(Type::LowPass, hz(24_000.0), hz(48_000.0)),
        Err(FilterError::Parameters)
    ));
    assert!(matches!(
        super::coefficients(Type::PeakingEQ(f32::NAN), hz(250.0), hz(48_000.0)),
        Err(FilterError::Parameters)
    ));
}
