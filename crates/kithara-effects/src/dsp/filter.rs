use biquad::{Biquad, Coefficients, DirectForm1, Type};
use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};

struct Consts;

impl Consts {
    const BUTTERWORTH_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;
    const NYQUIST_FACTOR: f32 = 2.0;
    const PASSTHROUGH: Coefficients<f32> = Coefficients {
        a1: 0.0,
        a2: 0.0,
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
    };
}

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
        let allpass = crossover_freqs
            .iter()
            .skip(1)
            .map(|&freq| Section::new(biquad_coeffs(Type::AllPass, freq, sample_rate)))
            .collect();
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
        // Constant gains commute with each allpass. Sharing one section per
        // crossover also filters a moving gain, whose ramp is kept smooth by
        // GainBank instead of retaining quadratic per-band filter histories.
        let mut output = self
            .lowpass_scratch
            .first()
            .map_or(0.0, |&low| low * gains(0));
        for index in 1..self.lowpass.len() {
            output =
                self.allpass[index - 1].run(output) + self.lowpass_scratch[index] * gains(index);
        }
        high.mul_add(gains(self.lowpass.len()), output)
    }

    fn rebuild(&mut self) {
        for (index, &freq) in self.crossover_freqs.iter().enumerate() {
            self.lowpass[index] = Lr4::new(biquad_coeffs(Type::LowPass, freq, self.sample_rate));
            self.highpass[index] = Lr4::new(biquad_coeffs(Type::HighPass, freq, self.sample_rate));
        }
        for (filter, &freq) in self
            .allpass
            .iter_mut()
            .zip(self.crossover_freqs.iter().skip(1))
        {
            *filter = Section::new(biquad_coeffs(Type::AllPass, freq, self.sample_rate));
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
    let normalized = Consts::NYQUIST_FACTOR * freq / sample_rate;
    Coefficients::<f32>::from_normalized_params(filter, normalized, Consts::BUTTERWORTH_Q)
        .unwrap_or(Consts::PASSTHROUGH)
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
