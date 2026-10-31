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

/// What one curve needs while it runs. A spectrum is predicted from the two
/// frames before it, so these carry across frames within a call and nothing
/// beyond it.
struct Frames {
    spectrum: Spectrum,
    /// `[previous_re, previous_im, earlier_re, earlier_im]`.
    history: [SampleBuffer; 4],
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
            history: [
                self.pools.get_with_len::<f32>(bins)?,
                self.pools.get_with_len::<f32>(bins)?,
                self.pools.get_with_len::<f32>(bins)?,
                self.pools.get_with_len::<f32>(bins)?,
            ],
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
        spectrum::prediction_difference(
            [self.spectrum.re(), self.spectrum.im()],
            self.history.each_mut().map(|plane| &mut plane[..]),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

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

    fn complete_curve_bits<S: HasPool<f32>>(novelty: &Novelty<S>, pcm: &[f32]) -> Vec<u32> {
        let curve = novelty
            .curve(pcm)
            .expect("the complete curve fits the region");
        assert!(curve.iter().all(|value| value.is_finite()));
        curve.iter().map(|value| value.to_bits()).collect()
    }

    #[kithara::test(native)]
    fn complete_curves_are_independent_of_previous_audio(
        clicks_120_4s: Vec<f32>,
        click_silence_4s: Vec<f32>,
    ) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/track_excerpt_mono_22050.f32le");
        let bytes = fs::read(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        let (words, remainder) = bytes.as_chunks::<4>();
        assert!(
            remainder.is_empty(),
            "the fixture contains complete samples"
        );
        let frames = 22_050 * 30;
        assert!(
            words.len() >= frames,
            "the full benchmark excerpt is present"
        );
        let track: Vec<f32> = words[..frames]
            .iter()
            .map(|word| f32::from_le_bytes(*word))
            .collect();
        assert_eq!(clicks_120_4s.len(), 22_050 * 4);
        assert_eq!(click_silence_4s.len(), 22_050 * 4);

        let novelty = Novelty::new(pools()).expect("the novelty FFT length is supported");
        for (name, pcm, other, expected_len) in [
            (
                "track_30s",
                track.as_slice(),
                clicks_120_4s.as_slice(),
                2_581,
            ),
            (
                "clicks_120_4s",
                clicks_120_4s.as_slice(),
                click_silence_4s.as_slice(),
                343,
            ),
            (
                "silence_4s",
                click_silence_4s.as_slice(),
                track.as_slice(),
                343,
            ),
        ] {
            let first = complete_curve_bits(&novelty, pcm);
            // Drop pooled curves before the next call so their storage can be reused.
            let _ = complete_curve_bits(&novelty, other);
            let reused = complete_curve_bits(&novelty, pcm);
            let fresh = complete_curve_bits(
                &Novelty::new(pools()).expect("a fresh novelty transform builds"),
                pcm,
            );
            assert_eq!(first.len(), expected_len, "{name}: complete curve length");
            assert_eq!(
                first, reused,
                "{name}: another input changed the full curve"
            );
            assert_eq!(first, fresh, "{name}: fresh and reused curves differ");
        }
    }
}

#[cfg(test)]
mod oracle {
    use kithara_test_fixtures::unit_fixtures::{click_silence_4s, clicks_120_4s};
    use kithara_test_utils::kithara;
    use num_traits::cast::ToPrimitive;

    use super::{Frames, Novelty, consts, tests::peaks};
    use crate::test_pools::pools;

    /// The phase law in `f64`: each bin predicted from the previous magnitude
    /// and the previous phase advanced by the last step, a silent bin at phase
    /// zero. Returns the difference and the summed magnitudes of the current
    /// and previous bins, which scale its admissible error.
    fn difference(re: &[f32], im: &[f32], state: &mut [Vec<f64>; 3]) -> (f64, f64) {
        let [magnitudes, phases, steps] = state;
        let (mut total, mut scale) = (0.0, 0.0);
        for (((re, im), magnitude), (phase, step)) in re
            .iter()
            .zip(im)
            .zip(magnitudes.iter_mut())
            .zip(phases.iter_mut().zip(steps.iter_mut()))
        {
            let (re, im) = (f64::from(*re), f64::from(*im));
            let observed = re.hypot(im);
            let angle = if observed > 0.0 { im.atan2(re) } else { 0.0 };
            let (sin, cos) = (*phase + *step).sin_cos();
            total += (re - *magnitude * cos).hypot(im - *magnitude * sin);
            scale += observed + *magnitude;
            *step = angle - *phase;
            *phase = angle;
            *magnitude = observed;
        }
        (total, scale)
    }

    fn bits(values: &[f32]) -> Vec<u32> {
        values.iter().map(|v| v.to_bits()).collect()
    }

    /// The curve from the `f64` law with each point's error bound, checking the
    /// history planes after every frame.
    fn reference(mono: &[f32]) -> Vec<(f64, f64)> {
        if mono.len() < consts::FRAMES_FRAME {
            return Vec::new();
        }
        let owner = Novelty::new(pools()).expect("supported FFT");
        let bins = consts::NOVELTY_FFT.bins();
        let mut work = Frames {
            spectrum: owner.fft.spectrum(&owner.pools).expect("spectrum fits"),
            history: std::array::from_fn(|_| {
                owner.pools.get_with_len::<f32>(bins).expect("plane fits")
            }),
        };
        let mut spectrum = owner
            .fft
            .spectrum(&owner.pools)
            .expect("oracle spectrum fits");
        let mut state = std::array::from_fn(|_| vec![0.0; bins]);
        let mut previous = [vec![0.0_f32; bins], vec![0.0; bins]];
        let frames = (mono.len() - consts::FRAMES_FRAME) / consts::NOVELTY_STRIDE + 2;
        let mut coarse = Vec::new();
        for index in 0..frames {
            let at = index * consts::NOVELTY_STRIDE;
            let end = (at + consts::FRAMES_FRAME).min(mono.len());
            let frame = &mono[at..end];
            owner.fft.forward(frame, &mut spectrum).expect("frame fits");
            let (expected, scale) = difference(spectrum.re(), spectrum.im(), &mut state);
            let actual = f64::from(work.difference(&owner.fft, frame));
            let bound = 1e-5 * scale + f64::from(f32::MIN_POSITIVE);
            assert!(
                (actual - expected).abs() <= bound,
                "frame {index}, {} samples: {actual} vs {expected}, bound {bound}",
                mono.len()
            );
            let [previous_re, previous_im, earlier_re, earlier_im] = &work.history;
            assert_eq!(bits(earlier_re), bits(&previous[0]), "frame {index}");
            assert_eq!(bits(earlier_im), bits(&previous[1]), "frame {index}");
            assert_eq!(bits(previous_re), bits(spectrum.re()), "frame {index}");
            assert_eq!(bits(previous_im), bits(spectrum.im()), "frame {index}");
            previous = [spectrum.re().to_vec(), spectrum.im().to_vec()];
            coarse.push((expected, bound));
        }
        let before = work.history.each_ref().map(|plane| bits(plane));
        assert_eq!(
            work.difference(&owner.fft, &vec![1.0; consts::FRAMES_FRAME + 1])
                .to_bits(),
            0
        );
        assert_eq!(before, work.history.each_ref().map(|plane| bits(plane)));
        let mut curve = Vec::new();
        for pair in coarse.windows(2) {
            curve.push(pair[0]);
            curve.push((
                f64::midpoint(pair[0].0, pair[1].0),
                pair[0].1.max(pair[1].1),
            ));
        }
        curve.push(*coarse.last().expect("at least two frames"));
        curve
    }

    #[kithara::test(native)]
    fn complete_curves_follow_the_phase_law_within_its_bound(
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
            let law: Vec<f32> = expected
                .iter()
                .map(|(value, _)| value.to_f32().expect("the law stays in f32 range"))
                .collect();
            for _ in 0..2 {
                let actual = owner.curve(pcm).expect("curve fits");
                assert_eq!(actual.len(), expected.len());
                for (index, (actual, (expected, bound))) in actual.iter().zip(&expected).enumerate()
                {
                    assert!(
                        (f64::from(*actual) - expected).abs() <= *bound,
                        "point {index}: {actual} vs {expected}, bound {bound}"
                    );
                }
                assert_eq!(peaks(&actual), peaks(&law));
            }
        }
    }
}
