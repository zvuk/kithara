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
    delay_blend(newer: &[f32], older: &[f32], fraction: f32, output: &mut [f32]) -> usize {
        simd: crate::backend::delay::blend_kernel;
        scalar: crate::backend::delay::blend_scalar;
        entry: crate::backend::delay::api::blend;
        targets: [Scalar, Simd, Simd];
    }
    delay_diffuse(samples: &mut [f32], history: &[f32], gain: f32, writes: &mut [f32]) -> usize {
        simd: crate::backend::delay::diffuse_kernel;
        scalar: crate::backend::delay::diffuse_scalar;
        entry: crate::backend::delay::diffuse;
        targets: [Scalar, Simd, Simd];
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
