use super::{
    ffi::{VdspBiquadSetup, vDSP_biquad, vDSP_biquad_CreateSetup, vDSP_biquad_DestroySetup},
    vector::clear_f32,
};

pub struct BiquadFilter {
    setup: VdspBiquadSetup,
    delay: [f32; 4],
}

// SAFETY: BiquadFilter owns vDSP setup and delay state.
// SAFETY: Methods require &mut self, and Drop destroys the setup once.
unsafe impl Send for BiquadFilter {}

impl BiquadFilter {
    #[must_use]
    pub fn low_pass(sample_rate: f64, cutoff_hz: f64, q: f64) -> Option<Self> {
        let coefficients = rbj_low_pass_coefficients(sample_rate, cutoff_hz, q)?;
        // SAFETY: coefficients points at one [b0,b1,b2,a1,a2] biquad section.
        let setup = unsafe { vDSP_biquad_CreateSetup(coefficients.as_ptr(), 1) };
        if setup.is_null() {
            return None;
        }
        Some(Self {
            setup,
            delay: [0.0; 4],
        })
    }

    pub fn process(&mut self, source: &[f32], target: &mut [f32]) -> usize {
        let frames = source.len().min(target.len());
        if frames == 0 {
            return 0;
        }
        // SAFETY: setup is live; delay has the 2*M+2 shape for one biquad section.
        // SAFETY: source and target each hold frames contiguous f32 values.
        unsafe {
            vDSP_biquad(
                self.setup,
                self.delay.as_mut_ptr(),
                source.as_ptr(),
                1,
                target.as_mut_ptr(),
                1,
                frames,
            );
        }
        frames
    }

    pub fn reset(&mut self) {
        clear_f32(&mut self.delay);
    }
}

impl Drop for BiquadFilter {
    fn drop(&mut self) {
        if !self.setup.is_null() {
            // SAFETY: setup was returned by vDSP_biquad_CreateSetup and is destroyed exactly once.
            unsafe {
                vDSP_biquad_DestroySetup(self.setup);
            }
        }
    }
}

fn rbj_low_pass_coefficients(sample_rate: f64, cutoff_hz: f64, q: f64) -> Option<[f64; 5]> {
    if !sample_rate.is_finite() || !cutoff_hz.is_finite() || !q.is_finite() {
        return None;
    }
    if sample_rate <= 0.0 || cutoff_hz <= 0.0 || q <= 0.0 {
        return None;
    }
    let nyquist = sample_rate * 0.5;
    let cutoff = cutoff_hz.min(nyquist * 0.999);
    let omega = std::f64::consts::TAU * cutoff / sample_rate;
    let sin = omega.sin();
    let cos = omega.cos();
    let alpha = sin / (2.0 * q);
    let b0 = (1.0 - cos) * 0.5;
    let b1 = 1.0 - cos;
    let b2 = b0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos;
    let a2 = 1.0 - alpha;
    Some([b0 / a0, b1 / a0, b2 / a0, -a1 / a0, -a2 / a0])
}
