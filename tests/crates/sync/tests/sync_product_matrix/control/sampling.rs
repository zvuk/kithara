use num_traits::ToPrimitive;

pub(super) const SINC_RADIUS: i32 = 16;

pub(super) struct SampleKernel {
    fraction: f64,
    sinc: Option<([f64; (2 * SINC_RADIUS + 1) as usize], f64)>,
}

impl SampleKernel {
    pub(super) fn new(position: (u128, u128), speed: f64) -> Result<Self, String> {
        let fraction = (position.0 % position.1)
            .to_f64()
            .zip(position.1.to_f64())
            .map(|(remainder, denominator)| remainder / denominator)
            .ok_or_else(|| "source fraction is not representable".to_owned())?;
        let sinc = if speed <= 1.0 {
            None
        } else {
            let cutoff = speed.recip();
            let mut weights = [0.0; (2 * SINC_RADIUS + 1) as usize];
            let mut normalizer = 0.0;
            for (weight, offset) in weights.iter_mut().zip(-SINC_RADIUS..=SINC_RADIUS) {
                let distance = f64::from(offset) - fraction;
                *weight = kithara::dsp::interp::sinc_hann_weight(
                    distance,
                    cutoff,
                    f64::from(SINC_RADIUS + 1),
                );
                normalizer += *weight;
            }
            Some((weights, normalizer))
        };
        Ok(Self { fraction, sinc })
    }

    pub(super) fn samples<const N: usize>(
        &self,
        mut sample: impl FnMut(i64) -> Result<[f32; N], String>,
    ) -> Result<[f32; N], String> {
        let Some((weights, normalizer)) = &self.sinc else {
            if self.fraction == 0.0 {
                return sample(0);
            }
            let current = sample(0)?;
            let next = sample(1)?;
            let mut output = [0.0; N];
            for ((output, current), next) in output.iter_mut().zip(current).zip(next) {
                let current = f64::from(current);
                *output = (f64::from(next) - current)
                    .mul_add(self.fraction, current)
                    .to_f32()
                    .filter(|sample| sample.is_finite())
                    .ok_or_else(|| "control source sample is not finite f32".to_owned())?;
            }
            return Ok(output);
        };
        let mut total = [0.0; N];
        for (offset, &weight) in (-SINC_RADIUS..=SINC_RADIUS).zip(weights) {
            for (total, value) in total.iter_mut().zip(sample(i64::from(offset))?) {
                *total += f64::from(value) * weight;
            }
        }
        let mut output = [0.0; N];
        for (output, total) in output.iter_mut().zip(total) {
            *output = (total / normalizer)
                .to_f32()
                .filter(|sample| sample.is_finite())
                .ok_or_else(|| "control source sample is not finite f32".to_owned())?;
        }
        Ok(output)
    }
}

pub(super) fn scalar_reference(
    position: (u128, u128),
    speed: f64,
    mut sample: impl FnMut(i64) -> Result<f32, String>,
) -> Result<f32, String> {
    let fraction = (position.0 % position.1)
        .to_f64()
        .zip(position.1.to_f64())
        .map(|(remainder, denominator)| remainder / denominator)
        .ok_or_else(|| "source fraction is not representable".to_owned())?;
    if speed <= 1.0 {
        if fraction == 0.0 {
            return sample(0);
        }
        let current = f64::from(sample(0)?);
        return (f64::from(sample(1)?) - current)
            .mul_add(fraction, current)
            .to_f32()
            .filter(|sample| sample.is_finite())
            .ok_or_else(|| "control source sample is not finite f32".to_owned());
    }
    let cutoff = speed.recip();
    let mut total = 0.0;
    let mut weights = 0.0;
    for offset in -SINC_RADIUS..=SINC_RADIUS {
        let distance = f64::from(offset) - fraction;
        let weight =
            kithara::dsp::interp::sinc_hann_weight(distance, cutoff, f64::from(SINC_RADIUS + 1));
        total += f64::from(sample(i64::from(offset))?) * weight;
        weights += weight;
    }
    (total / weights)
        .to_f32()
        .filter(|sample| sample.is_finite())
        .ok_or_else(|| "control source sample is not finite f32".to_owned())
}

#[kithara::test]
fn mapped_samples_preserve_scalar_bits_and_read_errors() {
    let signals = [
        [0.0, -0.0, f32::from_bits(1), -f32::from_bits(1)],
        [0.125, -0.75, 1.25, -2.5],
        [f32::MAX, -f32::MAX, 0.0, 1.0],
        [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0],
    ];
    for position in [
        (0, 1),
        (128, 4),
        (129, 4),
        (131, 4),
        (100, 3),
        (u128::MAX - 2, u128::MAX),
    ] {
        for speed in [
            f64::NEG_INFINITY,
            -0.0,
            0.5,
            1.0,
            1.00001,
            1.5,
            8.0,
            f64::INFINITY,
            f64::NAN,
        ] {
            for signal in signals {
                for fail in [None, Some(-16), Some(0), Some(1), Some(16)] {
                    let read = |offset: i64, channel: usize| {
                        if fail == Some(offset) {
                            return Err(format!("unavailable source offset {offset}"));
                        }
                        let index = usize::try_from(position.0 / position.1 % 4)
                            .expect("source signal index below four");
                        let offset = usize::try_from(offset.rem_euclid(4))
                            .expect("source offset index below four");
                        Ok(signal[(index + offset + channel) % 4])
                    };
                    let mut actual_reads = Vec::new();
                    let actual = super::reference::mapped_samples(position, speed, |offset| {
                        actual_reads.push(offset);
                        Ok([read(offset, 0)?, read(offset, 1)?])
                    });
                    let mut expected = [0; 2];
                    let mut error = None;
                    for (channel, expected) in expected.iter_mut().enumerate() {
                        let mut reads = Vec::new();
                        let scalar = scalar_reference(position, speed, |offset| {
                            reads.push(offset);
                            read(offset, channel)
                        });
                        assert_eq!(
                            actual_reads, reads,
                            "{position:?}, {speed}, {channel}, {fail:?}"
                        );
                        match scalar {
                            Ok(sample) => *expected = sample.to_bits(),
                            Err(message) => error = Some(message),
                        }
                    }
                    let expected = error.map_or(Ok(expected), Err);
                    assert_eq!(
                        actual.map(|samples| samples.map(f32::to_bits)),
                        expected,
                        "{position:?}, {speed}, {signal:?}, {fail:?}"
                    );
                    let mono = super::reference::mapped_samples(position, speed, |offset| {
                        read(offset, 0).map(|sample| [sample])
                    });
                    assert_eq!(
                        mono.map(|[sample]| sample.to_bits()),
                        scalar_reference(position, speed, |offset| read(offset, 0))
                            .map(f32::to_bits)
                    );
                }
            }
        }
    }
}

#[kithara::test]
fn shared_phase_kernel_preserves_shifted_source_bits() {
    for position in [(128, 4), (129, 4), (131, 4), (100, 3)] {
        for speed in [0.5, 1.0, 1.00001, 1.5, 8.0, f64::INFINITY, f64::NAN] {
            let kernel = SampleKernel::new(position, speed).expect("representable source phase");
            for numerator in [
                position.0,
                position.0.checked_add(position.1).expect("shifted source"),
            ] {
                let position = (numerator, position.1);
                let read = |offset: i64, channel: i16| {
                    let frame = i16::try_from(position.0 / position.1)
                        .expect("small quadratic source frame")
                        + i16::try_from(offset).expect("bounded sinc tap");
                    Ok(f32::from(frame * frame + channel * 7) * 0.001)
                };
                let actual = kernel.samples(|offset| Ok([read(offset, 0)?, read(offset, 1)?]));
                let expected = [0, 1].map(|channel| {
                    scalar_reference(position, speed, |offset| read(offset, channel))
                        .map(f32::to_bits)
                });
                let expected = match expected {
                    [Ok(left), Ok(right)] => Ok([left, right]),
                    [Err(error), _] | [_, Err(error)] => Err(error),
                };
                assert_eq!(
                    actual.map(|samples| samples.map(f32::to_bits)),
                    expected,
                    "{position:?}, {speed}"
                );
            }
        }
    }
}
