use super::mechanism::routes;

routes! { @export
    ramp(output: &mut [f32], start: f32, step: f32) -> usize {
        simd: crate::backend::simd::ramp_kernel;
        entry: crate::ramp;
        targets: [Simd, Simd, Simd];
    }
    div(samples: &mut [f32], divisor: f32) -> usize {
        simd: crate::backend::simd::div_kernel;
        entry: crate::div;
        targets: [Simd, Simd, Simd];
    }
    mul_add(x: &[f32], y: &[f32], acc: &mut [f32]) -> usize {
        simd: crate::backend::simd::mul_add_kernel;
        entry: crate::mul_add;
        targets: [Simd, Simd, Simd];
    }
    ratio_ramp(output: &mut [f64], first: u64, divisor: std::num::NonZeroU64) -> usize {
        simd: crate::backend::curve::ratio::ratio_kernel;
        scalar: crate::backend::curve::ratio::ratio_scalar;
        entry: crate::ratio_ramp;
        targets: [Scalar, Simd, Simd];
    }
    parameter_hold(positions: &mut [f64], endpoints: [f64; 2]) -> () {
        simd: crate::backend::curve::parameter::hold_kernel;
        entry: crate::backend::curve::parameter::hold;
        targets: [Simd, Simd, Simd];
    }
    parameter_linear(positions: &mut [f64], endpoints: [f64; 2]) -> () {
        simd: crate::backend::curve::parameter::linear_kernel;
        entry: crate::backend::curve::parameter::linear;
        targets: [Simd, Simd, Simd];
    }
}

routes! { @export
    svf_pair[S: fearless_simd::Simd](
        simd: S,
        samples: &mut [Option<&mut f32>],
        state1: &mut [f64],
        state2: &mut [f64],
        coefficients: [f64; 3],
        damping: f64,
        response: crate::filter::Response,
    ) -> bool {
        simd: crate::backend::svf::step_pair;
        scalar: crate::backend::svf::step_pair_scalar;
        entry: crate::backend::svf::step_pair;
        targets: [Scalar, Simd, Simd];
    }
}
