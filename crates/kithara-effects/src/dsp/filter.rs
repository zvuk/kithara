use biquad::{Biquad, Coefficients, DirectForm1, Type};
use kithara_bufpool::SampleBuffer;

use crate::consts;

struct Section(DirectForm1<f32>);

impl Section {
    const fn new(coefficients: Coefficients<f32>) -> Self {
        Self(DirectForm1::new(coefficients))
    }

    #[inline]
    fn run(&mut self, input: f32) -> f32 {
        let out = self.0.run(input);
        if out.is_subnormal() && input.abs() < f32::MIN_POSITIVE {
            self.0.reset_state();
            return 0.0;
        }
        out
    }
}

struct Lr4 {
    first: Section,
    second: Section,
}

impl Lr4 {
    const fn new(coefficients: Coefficients<f32>) -> Self {
        Self {
            first: Section::new(coefficients),
            second: Section::new(coefficients),
        }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        self.second.run(self.first.run(input))
    }
}

pub(crate) struct CrossoverFilters {
    crossover_freqs: SampleBuffer,
    allpass: Vec<Section>,
    highpass: Vec<Lr4>,
    lowpass: Vec<Lr4>,
    sample_rate: f32,
}

impl CrossoverFilters {
    pub(crate) fn new(crossover_freqs: SampleBuffer, sample_rate: f32) -> Self {
        let lowpass = crossover_freqs
            .iter()
            .map(|&freq| Lr4::new(biquad_coeffs(Type::LowPass, freq, sample_rate)))
            .collect();
        let highpass = crossover_freqs
            .iter()
            .map(|&freq| Lr4::new(biquad_coeffs(Type::HighPass, freq, sample_rate)))
            .collect();
        let mut allpass: Vec<Section> = Vec::new();
        for start in 1..crossover_freqs.len() {
            allpass.extend(
                crossover_freqs[start..]
                    .iter()
                    .map(|&freq| Section::new(biquad_coeffs(Type::AllPass, freq, sample_rate))),
            );
        }
        Self {
            crossover_freqs,
            allpass,
            highpass,
            lowpass,
            sample_rate,
        }
    }

    pub(crate) fn process(&mut self, input: f32, gains: impl Fn(usize) -> f32) -> f32 {
        let mut high = input;
        let mut output = 0.0;
        let crossover_count = self.lowpass.len();
        let mut allpass = self.allpass.as_mut_slice();
        for (index, (lowpass, highpass)) in
            self.lowpass.iter_mut().zip(&mut self.highpass).enumerate()
        {
            let mut band = lowpass.process(high);
            high = highpass.process(high);
            let (filters, rest) = allpass.split_at_mut(crossover_count - index - 1);
            allpass = rest;
            for filter in filters {
                band = filter.run(band);
            }
            output = band.mul_add(gains(index), output);
        }
        high.mul_add(gains(crossover_count), output)
    }

    fn rebuild(&mut self) {
        for (index, &freq) in self.crossover_freqs.iter().enumerate() {
            self.lowpass[index] = Lr4::new(biquad_coeffs(Type::LowPass, freq, self.sample_rate));
            self.highpass[index] = Lr4::new(biquad_coeffs(Type::HighPass, freq, self.sample_rate));
        }
        let mut allpass_start = 0;
        for band in 0..self.lowpass.len() {
            let allpass_count = self.lowpass.len().saturating_sub(band + 1);
            let allpass_end = allpass_start + allpass_count;
            for (offset, filter) in self.allpass[allpass_start..allpass_end]
                .iter_mut()
                .enumerate()
            {
                let freq = self.crossover_freqs[band + 1 + offset];
                *filter = Section::new(biquad_coeffs(Type::AllPass, freq, self.sample_rate));
            }
            allpass_start = allpass_end;
        }
    }

    pub(crate) fn reset(&mut self) {
        self.rebuild();
    }

    pub(crate) fn update_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.rebuild();
    }
}

fn biquad_coeffs(filter: Type<f32>, freq: f32, sample_rate: f32) -> Coefficients<f32> {
    let normalized = consts::NYQUIST_FACTOR * freq / sample_rate;
    Coefficients::<f32>::from_normalized_params(filter, normalized, consts::BUTTERWORTH_Q)
        .unwrap_or(consts::PASSTHROUGH)
}

#[cfg(test)]
mod tests {
    use biquad::{Biquad, Type};
    use kithara_test_utils::kithara;

    use super::{CrossoverFilters, Section, biquad_coeffs};
    use crate::test_pools::pools;

    #[kithara::test]
    #[case(0)]
    #[case(1)]
    #[case(2)]
    #[case(9)]
    fn crossover_history_matches_the_two_pass_reference(#[case] crossovers: usize) {
        let pools = pools();
        let frequencies = || {
            let mut frequencies = pools
                .get_with_len::<f32>(crossovers)
                .expect("test frequencies");
            for (index, freq) in frequencies.iter_mut().enumerate() {
                let index = f32::from(u16::try_from(index).expect("bounded crossover"));
                *freq = 40.0 * 2.0_f32.powf(index);
            }
            frequencies
        };
        for rate in [44_100.0, 48_000.0] {
            let mut actual = CrossoverFilters::new(frequencies(), rate);
            let mut reference = CrossoverFilters::new(frequencies(), rate);
            let mut scratch = vec![0.0; crossovers];
            let levels = [0.25, -0.5, 0.75, -1.0, 0.0, f32::MIN_POSITIVE, -0.0];
            let gains = [0.0, 1.0, -0.75, 2.0, 0.125];
            for frame in 0..24_096 {
                if frame == 512 {
                    actual.reset();
                    reference.reset();
                }
                if frame == 2048 {
                    actual.update_sample_rate(96_000.0);
                    reference.update_sample_rate(96_000.0);
                }
                let input = if !(4096..20_000).contains(&frame) {
                    levels[frame % levels.len()]
                } else {
                    0.0
                };
                let gain = |band| gains[(band + frame / 1024) % gains.len()];
                let expected = two_pass(&mut reference, &mut scratch, input, gain);
                assert_eq!(
                    actual.process(input, gain).to_bits(),
                    expected.to_bits(),
                    "crossovers {crossovers}, rate {rate}, frame {frame}"
                );
            }
        }
    }

    fn two_pass(
        filters: &mut CrossoverFilters,
        scratch: &mut [f32],
        input: f32,
        gains: impl Fn(usize) -> f32,
    ) -> f32 {
        let mut high = input;
        for (index, low) in scratch.iter_mut().enumerate() {
            *low = original_lr4(&mut filters.lowpass[index], high);
            high = original_lr4(&mut filters.highpass[index], high);
        }
        let mut output = 0.0;
        let mut start = 0;
        for (index, &low) in scratch.iter().enumerate() {
            let mut band = low;
            let end = start + scratch.len().saturating_sub(index + 1);
            for filter in &mut filters.allpass[start..end] {
                band = original_section(filter, band);
            }
            start = end;
            output = band.mul_add(gains(index), output);
        }
        high.mul_add(gains(scratch.len()), output)
    }

    fn original_section(section: &mut Section, input: f32) -> f32 {
        let out = section.0.run(input);
        if out.is_subnormal() && input.abs() < f32::MIN_POSITIVE {
            section.0.reset_state();
            return 0.0;
        }
        out
    }

    fn original_lr4(filter: &mut super::Lr4, input: f32) -> f32 {
        let first = original_section(&mut filter.first, input);
        original_section(&mut filter.second, first)
    }

    #[kithara::test(timeout(::std::time::Duration::from_millis(2226)))]
    fn butterworth_lp_dc_gain_is_unity() {
        let coeffs = biquad_coeffs(Type::LowPass, 250.0, 44100.0);
        let dc_gain = (coeffs.b0 + coeffs.b1 + coeffs.b2) / (1.0 + coeffs.a1 + coeffs.a2);
        assert!((dc_gain - 1.0).abs() < 0.001);
    }
}
