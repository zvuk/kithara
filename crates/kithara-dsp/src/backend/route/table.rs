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
}
