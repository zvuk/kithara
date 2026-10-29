use biquad::{Biquad, Coefficients, DirectForm1, Type};
use kithara_test_utils::kithara;

use super::Crossover;

#[kithara::test]
fn butterworth_lp_dc_gain_is_unity() {
    let coeffs = super::coefficients(Type::LowPass, 250.0, 44100.0);
    let dc_gain = (coeffs.b0 + coeffs.b1 + coeffs.b2) / (1.0 + coeffs.a1 + coeffs.a2);
    assert!((dc_gain - 1.0).abs() < 0.001);
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

#[kithara::test]
fn reset_clears_active_history_and_keeps_the_design() {
    let frequencies = [250.0, 2_500.0];
    let mut actual = Crossover::new(&frequencies, 48_000.0);
    let mut retained = Crossover::new(&frequencies, 48_000.0);
    let mut fresh = Crossover::new(&frequencies, 48_000.0);
    let warm = [0.25, -0.5, 1.0, -0.125];
    let probe = [1.0, -0.5, 0.0, 0.25];
    let mut discarded = vec![vec![0.0; warm.len()]; 3];
    for filter in [&mut actual, &mut retained] {
        assert_eq!(filter.split(&warm, &mut discarded), warm.len());
    }
    actual.reset();
    let mut outputs: [Vec<Vec<f32>>; 3] = std::array::from_fn(|_| vec![vec![0.0; probe.len()]; 3]);
    for (filter, output) in [&mut actual, &mut retained, &mut fresh]
        .into_iter()
        .zip(&mut outputs)
    {
        assert_eq!(filter.split(&probe, output), probe.len());
    }
    let [after_reset, retained_bits, fresh_bits] = outputs.map(|planes| {
        planes
            .iter()
            .flatten()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>()
    });
    assert_ne!(
        retained_bits, fresh_bits,
        "the probe must observe retained history"
    );
    assert_eq!(after_reset, fresh_bits);
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
        &[0.0, 1.0e9][..],
        &[
            100.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 6_000.0, 9_000.0, 12_000.0,
        ][..],
    ] {
        let mut actual = Crossover::new(frequencies, 48_000.0);
        for rate in [48_000.0, 44_100.0, 0.0] {
            actual.update_sample_rate(rate);
            for reset in [false, true] {
                if reset {
                    actual.reset();
                }
                let mut scalar = Scalar::new(frequencies, rate);
                for samples in input.chunks(chunk) {
                    let count = frequencies
                        .len()
                        .checked_add(1)
                        .expect("one more band than crossovers");
                    let mut bands = vec![vec![f32::NAN; samples.len()]; count];
                    assert_eq!(actual.split(samples, &mut bands), samples.len());
                    for (frame, &sample) in samples.iter().enumerate() {
                        for (band, expected) in bands.iter().zip(scalar.sample(sample)) {
                            assert_eq!(
                                band.get(frame).expect("processed band frame").to_bits(),
                                expected.to_bits(),
                                "rate {rate} chunk {chunk} frame {frame}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[kithara::test]
fn short_planes_extra_planes_and_empty_calls_do_not_advance_unprocessed_state() {
    let frequencies = [250.0, 2_500.0];
    let mut actual = Crossover::new(&frequencies, 48_000.0);
    let mut reference = Crossover::new(&frequencies, 48_000.0);
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
