use fearless_simd::Simd;
use kithara_apple::accelerate::tangent_f64;

use crate::{backend::svf::cutoff_angles, filter::SVF_CHUNK_FRAMES};

pub(crate) fn svf_gains<S: Simd>(simd: S, cutoffs: &[f64], rate: f64, gains: &mut [f64]) {
    let mut angles = [0.0; SVF_CHUNK_FRAMES];
    for (cutoffs, gains) in cutoffs
        .chunks(SVF_CHUNK_FRAMES)
        .zip(gains.chunks_mut(SVF_CHUNK_FRAMES))
    {
        let len = cutoff_angles(simd, cutoffs, rate, &mut angles[..gains.len()]);
        let _ = tangent_f64(&angles[..len], &mut gains[..len]);
    }
}
