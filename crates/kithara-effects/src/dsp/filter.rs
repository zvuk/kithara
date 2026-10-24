use biquad::{Biquad, Coefficients, DirectForm1, Type};
use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};

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
    lowpass_scratch: SampleBuffer,
    allpass: Vec<Section>,
    highpass: Vec<Lr4>,
    lowpass: Vec<Lr4>,
    sample_rate: f32,
}

impl CrossoverFilters {
    pub(crate) fn new<S>(
        pools: &PoolRegion<S>,
        crossover_freqs: SampleBuffer,
        sample_rate: f32,
    ) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
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
        let lowpass_scratch = pools.get_with_len::<f32>(crossover_freqs.len())?;
        Ok(Self {
            crossover_freqs,
            lowpass_scratch,
            allpass,
            highpass,
            lowpass,
            sample_rate,
        })
    }

    pub(crate) fn process(&mut self, input: f32, gains: impl Fn(usize) -> f32) -> f32 {
        let mut high = input;
        for index in 0..self.lowpass.len() {
            self.lowpass_scratch[index] = self.lowpass[index].process(high);
            high = self.highpass[index].process(high);
        }
        let mut output = 0.0;
        let mut allpass_start = 0;
        for index in 0..self.lowpass.len() {
            let mut band = self.lowpass_scratch[index];
            let allpass_count = self.lowpass.len().saturating_sub(index + 1);
            let allpass_end = allpass_start + allpass_count;
            for filter in &mut self.allpass[allpass_start..allpass_end] {
                band = filter.run(band);
            }
            allpass_start = allpass_end;
            output = band.mul_add(gains(index), output);
        }
        high.mul_add(gains(self.lowpass.len()), output)
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
    use biquad::Type;
    use kithara_test_utils::kithara;

    use super::biquad_coeffs;

    #[kithara::test]
    fn butterworth_lp_dc_gain_is_unity() {
        let coeffs = biquad_coeffs(Type::LowPass, 250.0, 44100.0);
        let dc_gain = (coeffs.b0 + coeffs.b1 + coeffs.b2) / (1.0 + coeffs.a1 + coeffs.a2);
        assert!((dc_gain - 1.0).abs() < 0.001);
    }
}
