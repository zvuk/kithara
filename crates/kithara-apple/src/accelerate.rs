use std::ptr;

type VdspStride = isize;
type VdspLength = usize;

#[link(name = "Accelerate", kind = "framework")]
unsafe extern "C" {
    fn cblas_scopy(n: i32, x: *const f32, inc_x: i32, y: *mut f32, inc_y: i32);
    fn vDSP_biquad(
        setup: VdspBiquadSetup,
        delay: *mut f32,
        x: *const f32,
        ix: VdspStride,
        y: *mut f32,
        iy: VdspStride,
        n: VdspLength,
    );
    fn vDSP_biquad_CreateSetup(coefficients: *const f64, order: VdspLength) -> VdspBiquadSetup;
    fn vDSP_biquad_DestroySetup(setup: VdspBiquadSetup);
    fn vDSP_vclr(c: *mut f32, ic: VdspStride, n: VdspLength);
    fn vDSP_vlint(
        a: *const f32,
        b: *const f32,
        ib: VdspStride,
        c: *mut f32,
        ic: VdspStride,
        n: VdspLength,
        m: VdspLength,
    );
    fn vDSP_vqint(
        a: *const f32,
        b: *const f32,
        ib: VdspStride,
        c: *mut f32,
        ic: VdspStride,
        n: VdspLength,
        m: VdspLength,
    );
    fn vDSP_vramp(a: *const f32, b: *const f32, c: *mut f32, ic: VdspStride, n: VdspLength);
}

type VdspBiquadSetup = *mut std::ffi::c_void;

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

pub fn copy_f32(source: &[f32], target: &mut [f32]) -> usize {
    let frames = source.len().min(target.len());
    if frames == 0 {
        return 0;
    }
    for offset in (0..frames).step_by(i32::MAX as usize) {
        let len = frames.saturating_sub(offset).min(i32::MAX as usize);
        let Ok(len_i32) = i32::try_from(len) else {
            break;
        };
        // SAFETY: offset and len are bounded by source and target lengths.
        // SAFETY: source and target strides are one element.
        unsafe {
            cblas_scopy(
                len_i32,
                source.as_ptr().add(offset),
                1,
                target.as_mut_ptr().add(offset),
                1,
            );
        }
    }
    frames
}

pub fn clear_f32(target: &mut [f32]) {
    if target.is_empty() {
        return;
    }
    // SAFETY: target points at target.len() contiguous f32 values with stride one.
    unsafe {
        vDSP_vclr(target.as_mut_ptr(), 1, target.len());
    }
}

pub fn ramp_f32(start: f32, step: f32, target: &mut [f32]) {
    if target.is_empty() {
        return;
    }
    let start_ptr = ptr::from_ref(&start);
    let step_ptr = ptr::from_ref(&step);
    // SAFETY: target points at target.len() contiguous f32 values.
    // SAFETY: scalar inputs are valid for the synchronous call.
    unsafe {
        vDSP_vramp(start_ptr, step_ptr, target.as_mut_ptr(), 1, target.len());
    }
}

pub fn linear_interpolate_f32(source: &[f32], positions: &[f32], target: &mut [f32]) -> usize {
    interpolate(source, positions, target, InterpolationKind::Linear)
}

pub fn quadratic_interpolate_f32(source: &[f32], positions: &[f32], target: &mut [f32]) -> usize {
    interpolate(source, positions, target, InterpolationKind::Quadratic)
}

fn interpolate(
    source: &[f32],
    positions: &[f32],
    target: &mut [f32],
    kind: InterpolationKind,
) -> usize {
    let frames = positions.len().min(target.len());
    if frames == 0 || source.is_empty() {
        return 0;
    }
    match kind {
        InterpolationKind::Linear => {
            // SAFETY: source, positions, and target point at contiguous f32 buffers.
            // SAFETY: frames bounds positions and target.
            // SAFETY: source.len() is supplied as the interpolation source length.
            unsafe {
                vDSP_vlint(
                    source.as_ptr(),
                    positions.as_ptr(),
                    1,
                    target.as_mut_ptr(),
                    1,
                    frames,
                    source.len(),
                );
            }
        }
        InterpolationKind::Quadratic => {
            // SAFETY: same contract as the linear path.
            unsafe {
                vDSP_vqint(
                    source.as_ptr(),
                    positions.as_ptr(),
                    1,
                    target.as_mut_ptr(),
                    1,
                    frames,
                    source.len(),
                );
            }
        }
    }
    frames
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

#[derive(Clone, Copy)]
enum InterpolationKind {
    Linear,
    Quadratic,
}

#[cfg(test)]
mod tests {
    use super::{BiquadFilter, clear_f32, copy_f32, ramp_f32};

    #[test]
    fn copy_f32_matches_slice_copy() {
        let source = [1.0, -2.0, 3.5, 4.25];
        let mut target = [0.0; 4];
        assert_eq!(copy_f32(&source, &mut target), source.len());
        assert_eq!(target, source);
    }

    #[test]
    fn clear_f32_sets_zero() {
        let mut target = [1.0, -2.0, 3.0];
        clear_f32(&mut target);
        assert_eq!(target, [0.0; 3]);
    }

    #[test]
    fn ramp_f32_matches_scalar_ramp() {
        let mut target = [0.0; 4];
        ramp_f32(0.5, 0.25, &mut target);
        assert_eq!(target, [0.5, 0.75, 1.0, 1.25]);
    }

    #[test]
    fn interpolation_outputs_requested_frames() {
        let source = [0.0, 1.0, 2.0, 3.0];
        let positions = [0.0, 0.5, 1.0];
        let mut target = [0.0; 3];
        assert_eq!(
            super::linear_interpolate_f32(&source, &positions, &mut target),
            target.len()
        );
    }

    #[test]
    fn linear_interpolation_matches_scalar_positions() {
        let source = [0.0, 1.0, 2.0, 3.0];
        let positions = [1.0, 1.25, 1.5, 1.75];
        let mut target = [0.0; 4];

        super::linear_interpolate_f32(&source, &positions, &mut target);

        assert_eq!(target, [1.0, 1.25, 1.5, 1.75]);
    }

    #[test]
    fn quadratic_interpolation_matches_scalar_positions() {
        let source = [0.0, 1.0, 0.0, -1.0, 0.0];
        let positions = [1.0, 1.25, 1.5, 1.75];
        let mut target = [0.0; 4];

        super::quadratic_interpolate_f32(&source, &positions, &mut target);

        let expected = [1.0, 0.9375, 0.75, 0.4375];
        for (actual, expected) in target.iter().zip(expected) {
            assert!((actual - expected).abs() < 0.000_001);
        }
    }

    #[test]
    fn biquad_low_pass_processes_requested_frames() {
        let Some(mut filter) =
            BiquadFilter::low_pass(44_100.0, 12_000.0, std::f64::consts::FRAC_1_SQRT_2)
        else {
            panic!("valid low pass filter");
        };
        let source = [0.0, 1.0, 0.0, -1.0, 0.0];
        let mut target = [0.0; 5];
        assert_eq!(filter.process(&source, &mut target), source.len());
        assert!(target.iter().all(|sample| f32::is_finite(*sample)));
    }
}
