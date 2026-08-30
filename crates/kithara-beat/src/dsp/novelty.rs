use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_platform::sync::Arc;
use num_traits::cast::ToPrimitive;
use realfft::{RealFftPlanner, RealToComplex, num_complex::Complex};

use super::frames::{FRAME, HOP};

const HANN_A0: f32 = 0.5;

/// Rectified complex spectral difference, one value per [`HOP`].
pub(crate) struct Novelty<S>
where
    S: HasPool<f32>,
{
    pools: PoolRegion<S>,
    fft: Arc<dyn RealToComplex<f32>>,
    hann: SampleBuffer,
}

/// What one curve needs while it runs. A spectrum is predicted from the frame
/// before it, so these carry across frames within a call and nothing beyond it.
struct Frames {
    input: SampleBuffer,
    magnitude: SampleBuffer,
    phase: SampleBuffer,
    phase_step: SampleBuffer,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl<S> Novelty<S>
where
    S: HasPool<f32>,
{
    pub(crate) fn new(pools: PoolRegion<S>) -> Result<Self, PoolError> {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FRAME);
        let hann = hann_window(&pools)?;
        Ok(Self { pools, fft, hann })
    }

    pub(crate) fn curve(&self, mono: &[f32]) -> Result<SampleBuffer, PoolError> {
        let frames = mono.len().saturating_sub(FRAME) / HOP + usize::from(mono.len() >= FRAME);
        let mut curve = self.pools.get_with_len::<f32>(frames)?;
        let bins = self.fft.make_output_vec().len();
        let mut work = Frames {
            input: self.pools.get_with_len::<f32>(FRAME)?,
            magnitude: self.pools.get_with_len::<f32>(bins)?,
            phase: self.pools.get_with_len::<f32>(bins)?,
            phase_step: self.pools.get_with_len::<f32>(bins)?,
            output: self.fft.make_output_vec(),
            scratch: self.fft.make_scratch_vec(),
        };
        for index in 0..frames {
            let at = index * HOP;
            work.input
                .iter_mut()
                .zip(mono[at..at + FRAME].iter().zip(self.hann.iter()))
                .for_each(|(slot, (sample, window))| *slot = sample * window);
            curve[index] = work.difference(&self.fft, index < 2);
        }
        Ok(curve)
    }
}

impl Frames {
    fn difference(&mut self, fft: &Arc<dyn RealToComplex<f32>>, warming: bool) -> f32 {
        if fft
            .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch)
            .is_err()
        {
            return 0.0;
        }
        let mut total = 0.0;
        for (bin, ((magnitude, phase), step)) in self.output.iter().zip(
            self.magnitude
                .iter_mut()
                .zip(self.phase.iter_mut())
                .zip(self.phase_step.iter_mut()),
        ) {
            let (observed, angle) = (bin.norm(), bin.arg());
            if observed >= *magnitude {
                let predicted = Complex::from_polar(*magnitude, wrap(*phase + *step));
                total += (bin - predicted).norm();
            }
            *step = wrap(angle - *phase);
            *phase = angle;
            *magnitude = observed;
        }
        if warming { 0.0 } else { total }
    }
}

fn wrap(angle: f32) -> f32 {
    let turn = std::f32::consts::TAU;
    angle - turn * (angle / turn).round()
}

fn hann_window<S>(pools: &PoolRegion<S>) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    let mut hann = pools.get_with_len::<f32>(FRAME)?;
    let denom = (FRAME - 1).to_f32().unwrap_or(1.0);
    let scale = std::f32::consts::TAU / denom;
    for (n, sample) in hann.iter_mut().enumerate() {
        let phase = scale * n.to_f32().unwrap_or(0.0);
        *sample = HANN_A0.mul_add(-phase.cos(), HANN_A0);
    }
    Ok(hann)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::pools;

    fn novelty() -> Novelty<impl HasPool<f32>> {
        Novelty::new(pools()).expect("a fresh region has room for the window")
    }
    use crate::dsp::{clicks, frames};

    fn peaks(curve: &[f32]) -> Vec<usize> {
        let ceiling = curve.iter().copied().fold(0.0f32, f32::max);
        (1..curve.len().saturating_sub(1))
            .filter(|&i| {
                curve[i] > ceiling * 0.4 && curve[i] >= curve[i - 1] && curve[i] > curve[i + 1]
            })
            .collect()
    }

    #[kithara::test(native, flash(false))]
    fn clicks_raise_peaks_where_the_clicks_are() {
        let pcm = clicks::track(4.0, 0.5);
        let curve = novelty().curve(&pcm).expect("the curve fits the region");

        let found: Vec<f32> = peaks(&curve)
            .into_iter()
            .map(|i| frames::seconds(i.to_f32().unwrap_or(0.0)))
            .collect();
        // A click inside the first window has nothing to be differenced
        // against, so the curve cannot carry it.
        let first_observable = FRAME.to_f32().unwrap_or(0.0) / frames::RATE;
        let expected: Vec<f32> = clicks::positions(4.0, 0.5)
            .into_iter()
            .filter(|at| *at >= first_observable)
            .collect();
        assert_eq!(
            found.len(),
            expected.len(),
            "one novelty peak per click: {found:?} vs {expected:?}"
        );
        for (got, want) in found.iter().zip(expected.iter()) {
            assert!(
                (got - want).abs() < 0.03,
                "peak at {got} s should land on the click at {want} s"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn silence_is_flat() {
        let curve = novelty().curve(&clicks::silence(4.0)).expect("the curve fits the region");
        assert!(!curve.is_empty(), "silence still yields a curve");
        assert!(
            curve.iter().all(|&v| v == 0.0),
            "silence has no spectral change"
        );
    }
}
