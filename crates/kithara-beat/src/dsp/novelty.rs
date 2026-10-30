use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use kithara_dsp::spectrum::{self, Fft, Spectrum, SpectrumError};

use super::consts;

/// Complex spectral difference, one value per [`consts::FRAMES_HOP`].
pub(crate) struct Novelty<S>
where
    S: HasPool<f32>,
{
    fft: Fft,
    pools: PoolRegion<S>,
}

/// What one curve needs while it runs. A spectrum is predicted from the frame
/// before it, so these carry across frames within a call and nothing beyond it.
struct Frames {
    spectrum: Spectrum,
    observed: SampleBuffer,
    angle: SampleBuffer,
    magnitude: SampleBuffer,
    phase: SampleBuffer,
    phase_step: SampleBuffer,
}

impl<S> Novelty<S>
where
    S: HasPool<f32>,
{
    pub(crate) fn new(pools: PoolRegion<S>) -> Result<Self, SpectrumError> {
        Ok(Self {
            fft: Fft::new(consts::NOVELTY_FFT)?,
            pools,
        })
    }

    /// The difference is measured every [`consts::NOVELTY_STRIDE`]
    /// samples and interpolated onto the [`consts::FRAMES_HOP`] grid,
    /// the resolution the reference reaches the same way. The last window
    /// is filled out with zeros, and the first window that needs that is
    /// the last.
    pub(crate) fn curve(&self, mono: &[f32]) -> Result<SampleBuffer, PoolError> {
        if mono.len() < consts::FRAMES_FRAME {
            return Ok(self.pools.get::<f32>());
        }
        let frames = (mono.len() - consts::FRAMES_FRAME) / consts::NOVELTY_STRIDE + 2;
        let mut coarse = self.pools.get_with_len::<f32>(frames)?;
        let bins = consts::NOVELTY_FFT.bins();
        let mut work = Frames {
            spectrum: self.fft.spectrum(&self.pools)?,
            observed: self.pools.get_with_len::<f32>(bins)?,
            angle: self.pools.get_with_len::<f32>(bins)?,
            magnitude: self.pools.get_with_len::<f32>(bins)?,
            phase: self.pools.get_with_len::<f32>(bins)?,
            phase_step: self.pools.get_with_len::<f32>(bins)?,
        };
        for (index, slot) in coarse.iter_mut().enumerate() {
            let at = index * consts::NOVELTY_STRIDE;
            let end = (at + consts::FRAMES_FRAME).min(mono.len());
            *slot = work.difference(&self.fft, &mono[at..end]);
        }
        let mut curve = self.pools.get_with_len::<f32>(
            (coarse.len() - 1) * (consts::NOVELTY_STRIDE / consts::FRAMES_HOP) + 1,
        )?;
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
    /// Distance between this frame's spectrum and the one predicted from the
    /// two before it; `0.0` when the frame does not fit the transform.
    fn difference(&mut self, fft: &Fft, frame: &[f32]) -> f32 {
        if fft.forward(frame, &mut self.spectrum).is_err() {
            return 0.0;
        }
        let (re, im) = (self.spectrum.re(), self.spectrum.im());
        let _ = spectrum::magnitude(re, im, &mut self.observed);
        let _ = spectrum::phase(re, im, &mut self.angle);
        spectrum::prediction_difference(
            [re, im],
            [&self.observed, &self.angle],
            [&mut self.magnitude, &mut self.phase, &mut self.phase_step],
        )
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_fixtures::unit_fixtures::{click_silence_4s, clicks_120_4s};
    use kithara_test_utils::kithara;
    use num_traits::cast::ToPrimitive;

    use super::*;
    use crate::{
        dsp::{clicks, frames},
        test_pools::pools,
    };

    pub(super) fn peaks(curve: &[f32]) -> Vec<usize> {
        let ceiling = curve.iter().copied().fold(0.0f32, f32::max);
        (1..curve.len().saturating_sub(1))
            .filter(|&i| {
                curve[i] > ceiling * 0.4 && curve[i] >= curve[i - 1] && curve[i] > curve[i + 1]
            })
            .collect()
    }

    #[kithara::test(native, flash(false))]
    fn clicks_raise_peaks_where_the_clicks_are(clicks_120_4s: Vec<f32>) {
        let pcm = clicks_120_4s;
        let curve = Novelty::new(pools())
            .expect("the novelty FFT length is supported")
            .curve(&pcm)
            .expect("the curve fits the region");

        let found: Vec<f32> = peaks(&curve)
            .into_iter()
            .map(|i| frames::seconds(i.to_f32().unwrap_or(0.0)))
            .collect();
        // The click at zero lands on the first frame, where a peak has no
        // left neighbour to stand above.
        let first_observable = consts::FRAMES_FRAME.to_f32().unwrap_or(0.0) / consts::FRAMES_RATE;
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
    fn silence_is_flat(click_silence_4s: Vec<f32>) {
        let curve = Novelty::new(pools())
            .expect("the novelty FFT length is supported")
            .curve(&click_silence_4s)
            .expect("the curve fits the region");
        assert!(!curve.is_empty(), "silence still yields a curve");
        assert!(
            curve.iter().all(|&v| v == 0.0),
            "silence has no spectral change"
        );
    }
}

#[cfg(test)]
mod oracle {
    use std::f32::consts::TAU;

    use kithara_test_fixtures::unit_fixtures::{click_silence_4s, clicks_120_4s};
    use kithara_test_utils::kithara;

    use super::{Frames, Novelty, consts, spectrum, tests::peaks};
    use crate::test_pools::pools;

    // Retain the original zipped loop: bounds checks can split the paired sin_cos call.
    fn difference(
        re: &[f32],
        im: &[f32],
        observed: &[f32],
        angle: &[f32],
        state: &mut [Vec<f32>; 3],
    ) -> f32 {
        let [magnitudes, phases, phase_steps] = state;
        let mut total = 0.0_f32;
        for ((((re, im), (observed, angle)), (magnitude, phase)), step) in re
            .iter()
            .zip(im)
            .zip(observed.iter().zip(angle))
            .zip(magnitudes.iter_mut().zip(phases.iter_mut()))
            .zip(phase_steps.iter_mut())
        {
            let (sin, cos) = wrap(*phase + *step).sin_cos();
            total += (re - *magnitude * cos).hypot(im - *magnitude * sin);
            *step = wrap(angle - *phase);
            *phase = *angle;
            *magnitude = *observed;
        }
        total
    }

    fn wrap(angle: f32) -> f32 {
        angle - TAU * (angle / TAU).round()
    }

    fn bits(values: &[f32]) -> Vec<u32> {
        values.iter().map(|v| v.to_bits()).collect()
    }

    fn reference(mono: &[f32]) -> Vec<f32> {
        if mono.len() < consts::FRAMES_FRAME {
            return Vec::new();
        }
        let owner = Novelty::new(pools()).expect("supported FFT");
        let bins = consts::NOVELTY_FFT.bins();
        let mut work = Frames {
            spectrum: owner.fft.spectrum(&owner.pools).expect("spectrum fits"),
            observed: owner.pools.get_with_len::<f32>(bins).expect("plane fits"),
            angle: owner.pools.get_with_len::<f32>(bins).expect("plane fits"),
            magnitude: owner.pools.get_with_len::<f32>(bins).expect("plane fits"),
            phase: owner.pools.get_with_len::<f32>(bins).expect("plane fits"),
            phase_step: owner.pools.get_with_len::<f32>(bins).expect("plane fits"),
        };
        let mut spectrum = owner
            .fft
            .spectrum(&owner.pools)
            .expect("oracle spectrum fits");
        let mut observed = vec![0.0; bins];
        let mut angle = vec![0.0; bins];
        let mut state = std::array::from_fn(|_| vec![0.0; bins]);
        let frames = (mono.len() - consts::FRAMES_FRAME) / consts::NOVELTY_STRIDE + 2;
        let mut coarse = Vec::new();
        for index in 0..frames {
            let at = index * consts::NOVELTY_STRIDE;
            let end = (at + consts::FRAMES_FRAME).min(mono.len());
            let frame = &mono[at..end];
            for (actual, expected) in [&work.magnitude, &work.phase, &work.phase_step]
                .into_iter()
                .zip(&state)
            {
                assert_eq!(bits(actual), bits(expected), "frame {index}: prior state");
            }
            owner.fft.forward(frame, &mut spectrum).expect("frame fits");
            spectrum::magnitude(spectrum.re(), spectrum.im(), &mut observed);
            spectrum::phase(spectrum.re(), spectrum.im(), &mut angle);
            let expected = difference(spectrum.re(), spectrum.im(), &observed, &angle, &mut state);
            let actual = work.difference(&owner.fft, frame);
            let planes = [
                ("real", work.spectrum.re(), spectrum.re()),
                ("imaginary", work.spectrum.im(), spectrum.im()),
                ("magnitude", &work.observed, &observed),
                ("phase", &work.angle, &angle),
            ];
            for (name, actual_plane, expected_plane) in planes {
                assert_eq!(actual_plane.len(), expected_plane.len());
                let first_difference = actual_plane
                    .iter()
                    .zip(expected_plane)
                    .enumerate()
                    .find_map(|(bin, (actual, expected))| {
                        (actual.to_bits() != expected.to_bits()).then_some((
                            bin,
                            actual.to_bits(),
                            expected.to_bits(),
                        ))
                    });
                assert_eq!(first_difference, None, "frame {index}: {name} input plane");
            }
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "frame {index}, {} samples: {actual} vs {expected}",
                mono.len()
            );
            for (actual, expected) in [&work.magnitude, &work.phase, &work.phase_step]
                .into_iter()
                .zip(&state)
            {
                assert_eq!(
                    bits(actual),
                    bits(expected),
                    "frame {index}: entire history plane"
                );
            }
            coarse.push(expected);
        }
        let before = [&work.magnitude, &work.phase, &work.phase_step].map(|s| bits(s));
        assert_eq!(
            work.difference(&owner.fft, &vec![1.0; consts::FRAMES_FRAME + 1])
                .to_bits(),
            0
        );
        assert_eq!(
            before,
            [&work.magnitude, &work.phase, &work.phase_step].map(|s| bits(s))
        );
        let mut curve = Vec::new();
        for pair in coarse.windows(2) {
            curve.push(pair[0]);
            curve.push((pair[0] + pair[1]) / 2.0);
        }
        curve.push(*coarse.last().expect("at least two frames"));
        curve
    }

    #[kithara::test(native, flash(false))]
    fn complete_curves_and_frame_history_match_scalar_law(
        clicks_120_4s: Vec<f32>,
        click_silence_4s: Vec<f32>,
    ) {
        let owner = Novelty::new(pools()).expect("supported FFT");
        let tone: Vec<f32> = std::iter::successors(Some(0.0_f32), |phase| Some(phase + 0.17))
            .take(4097)
            .map(f32::sin)
            .collect();
        for pcm in [
            &clicks_120_4s[..],
            &click_silence_4s,
            &tone,
            &tone[..1023],
            &tone[..1024],
            &tone[..1537],
        ] {
            let expected = reference(pcm);
            for _ in 0..2 {
                let actual = owner.curve(pcm).expect("curve fits");
                assert_eq!(bits(&actual), bits(&expected));
                assert_eq!(peaks(&actual), peaks(&expected));
            }
        }
    }
}
