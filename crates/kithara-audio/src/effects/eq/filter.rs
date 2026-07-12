use biquad::{Biquad, Coefficients, DirectForm1, Type};

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

struct Lr4 {
    first: DirectForm1<f32>,
    second: DirectForm1<f32>,
}

impl Lr4 {
    fn new(coefficients: Coefficients<f32>) -> Self {
        Self {
            first: DirectForm1::new(coefficients),
            second: DirectForm1::new(coefficients),
        }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        self.second.run(self.first.run(input))
    }
}

pub(crate) struct CrossoverFilters {
    allpass: Vec<DirectForm1<f32>>,
    allpass_offsets: Vec<usize>,
    history: Vec<f32>,
    history_pos: usize,
    crossover_freqs: Vec<f32>,
    highpass: Vec<Lr4>,
    lowpass: Vec<Lr4>,
    lowpass_scratch: Vec<f32>,
    sample_rate: f32,
}

impl CrossoverFilters {
    const HISTORY_LEN: usize = 128;

    pub(crate) fn new(crossover_freqs: Vec<f32>, band_count: usize, sample_rate: f32) -> Self {
        let lowpass = crossover_freqs
            .iter()
            .map(|&freq| Lr4::new(biquad_coeffs(Type::LowPass, freq, sample_rate)))
            .collect();
        let highpass = crossover_freqs
            .iter()
            .map(|&freq| Lr4::new(biquad_coeffs(Type::HighPass, freq, sample_rate)))
            .collect();
        let mut allpass = Vec::new();
        let mut allpass_offsets = Vec::with_capacity(band_count + 1);
        for band in 0..band_count {
            allpass_offsets.push(allpass.len());
            if band + 1 < crossover_freqs.len() {
                allpass.extend(crossover_freqs[band + 1..].iter().map(|&freq| {
                    DirectForm1::new(biquad_coeffs(Type::AllPass, freq, sample_rate))
                }));
            }
        }
        allpass_offsets.push(allpass.len());
        Self {
            allpass,
            allpass_offsets,
            history: vec![0.0; Self::HISTORY_LEN],
            history_pos: 0,
            lowpass_scratch: vec![0.0; crossover_freqs.len()],
            crossover_freqs,
            highpass,
            lowpass,
            sample_rate,
        }
    }

    pub(crate) fn process(&mut self, input: f32, gains: impl Fn(usize) -> f32) -> f32 {
        let mut high = input;
        for index in 0..self.lowpass.len() {
            self.lowpass_scratch[index] = self.lowpass[index].process(high);
            high = self.highpass[index].process(high);
        }
        let mut output = 0.0;
        for index in 0..self.lowpass.len() {
            let mut band = self.lowpass_scratch[index];
            for filter in
                &mut self.allpass[self.allpass_offsets[index]..self.allpass_offsets[index + 1]]
            {
                band = filter.run(band);
            }
            output += band * gains(index);
        }
        output + high * gains(self.lowpass.len())
    }

    pub(crate) fn record(&mut self, input: f32) {
        self.history[self.history_pos] = input;
        self.history_pos = (self.history_pos + 1) % self.history.len();
    }

    pub(crate) fn rehydrate(&mut self) {
        if self.lowpass.is_empty() {
            return;
        }
        for offset in 0..self.history.len() {
            let sample = self.history[(self.history_pos + offset) % self.history.len()];
            let mut high = sample;
            for index in 0..self.lowpass.len() {
                self.lowpass_scratch[index] = self.lowpass[index].process(high);
                high = self.highpass[index].process(high);
            }
            for index in 0..self.lowpass.len() {
                let mut band = self.lowpass_scratch[index];
                for filter in
                    &mut self.allpass[self.allpass_offsets[index]..self.allpass_offsets[index + 1]]
                {
                    band = filter.run(band);
                }
            }
        }
    }

    pub(crate) fn reset(&mut self) {
        self.rebuild();
        self.history.fill(0.0);
        self.history_pos = 0;
    }

    pub(crate) fn update_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.rebuild();
    }

    fn rebuild(&mut self) {
        for (index, &freq) in self.crossover_freqs.iter().enumerate() {
            self.lowpass[index] = Lr4::new(biquad_coeffs(Type::LowPass, freq, self.sample_rate));
            self.highpass[index] = Lr4::new(biquad_coeffs(Type::HighPass, freq, self.sample_rate));
        }
        for band in 0..self.lowpass.len() + 1 {
            let start = self.allpass_offsets[band];
            let end = self.allpass_offsets[band + 1];
            for (offset, filter) in self.allpass[start..end].iter_mut().enumerate() {
                let freq = self.crossover_freqs[band + 1 + offset];
                *filter = DirectForm1::new(biquad_coeffs(Type::AllPass, freq, self.sample_rate));
            }
        }
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
