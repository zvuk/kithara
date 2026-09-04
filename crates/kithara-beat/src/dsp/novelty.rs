use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_platform::sync::Arc;
use num_traits::cast::ToPrimitive;
use realfft::{RealFftPlanner, RealToComplex, num_complex::Complex};

use super::frames::{FRAME, HOP};

struct Consts;

impl Consts {
    const HANN_A0: f32 = 0.5;
    /// Analysis stride, 23.2 ms: the rate the difference is actually
    /// measured at.
    const STRIDE: usize = 2 * HOP;
}

/// Complex spectral difference, one value per [`HOP`].
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
        Ok(Self {
            hann: hann_window(&pools)?,
            fft,
            pools,
        })
    }

    /// The difference is measured every [`Consts::STRIDE`] samples and interpolated
    /// onto the [`HOP`] grid, the resolution the reference reaches the same
    /// way. The last window is filled out with zeros, and the first window
    /// that needs that is the last.
    pub(crate) fn curve(&self, mono: &[f32]) -> Result<SampleBuffer, PoolError> {
        if mono.len() < FRAME {
            return Ok(self.pools.get::<f32>());
        }
        let frames = (mono.len() - FRAME) / Consts::STRIDE + 2;
        let mut coarse = self.pools.get_with_len::<f32>(frames)?;
        let bins = self.fft.complex_len();
        let mut work = Frames {
            input: self.pools.get_with_len::<f32>(FRAME)?,
            magnitude: self.pools.get_with_len::<f32>(bins)?,
            phase: self.pools.get_with_len::<f32>(bins)?,
            phase_step: self.pools.get_with_len::<f32>(bins)?,
            output: self.fft.make_output_vec(),
            scratch: self.fft.make_scratch_vec(),
        };
        for (index, slot) in coarse.iter_mut().enumerate() {
            let at = index * Consts::STRIDE;
            let end = (at + FRAME).min(mono.len());
            let (signal, padding) = work.input.split_at_mut(end - at);
            signal
                .iter_mut()
                .zip(mono[at..end].iter().zip(self.hann.iter()))
                .for_each(|(sample_slot, (sample, window))| *sample_slot = sample * window);
            padding.fill(0.0);
            *slot = work.difference(&self.fft);
        }
        let mut curve = self
            .pools
            .get_with_len::<f32>((coarse.len() - 1) * (Consts::STRIDE / HOP) + 1)?;
        for (index, slot) in curve.iter_mut().enumerate() {
            let (whole, half) = (index / 2, index % 2 == 1);
            *slot = if half {
                (coarse[whole] + coarse[whole + 1]) / 2.0
            } else {
                coarse[whole]
            };
        }
        Ok(curve)
    }
}

impl Frames {
    fn difference(&mut self, fft: &Arc<dyn RealToComplex<f32>>) -> f32 {
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
            let predicted = Complex::from_polar(*magnitude, wrap(*phase + *step));
            total += (bin - predicted).norm();
            *step = wrap(angle - *phase);
            *phase = angle;
            *magnitude = observed;
        }
        total
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
        *sample = Consts::HANN_A0.mul_add(-phase.cos(), Consts::HANN_A0);
    }
    Ok(hann)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        dsp::{clicks, frames},
        test_pools::pools,
    };

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
        let curve = Novelty::new(pools())
            .expect("a fresh region has room for the window")
            .curve(&pcm)
            .expect("the curve fits the region");

        let found: Vec<f32> = peaks(&curve)
            .into_iter()
            .map(|i| frames::seconds(i.to_f32().unwrap_or(0.0)))
            .collect();
        // The click at zero lands on the first frame, where a peak has no
        // left neighbour to stand above.
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
        let curve = Novelty::new(pools())
            .expect("a fresh region has room for the window")
            .curve(&clicks::silence(4.0))
            .expect("the curve fits the region");
        assert!(!curve.is_empty(), "silence still yields a curve");
        assert!(
            curve.iter().all(|&v| v == 0.0),
            "silence has no spectral change"
        );
    }
}
