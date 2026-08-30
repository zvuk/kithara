use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use num_traits::cast::ToPrimitive;

use super::{decode, frames, novelty::Novelty, period};
use crate::mark::{BeatMark, RawBeats};

/// A mark is never a certainty, and never nothing.
const CONFIDENCE_BOUNDS: (f32, f32) = (0.001, 0.999);

/// Signal-processing beat detector: novelty curve, comb-filtered period,
/// then beats decoded over inter-beat intervals. No model data, no network.
///
/// Reports beats and never downbeats: this family of tracker does not
/// establish bar starts.
pub struct SpectralBeats<S>
where
    S: HasPool<f32>,
{
    novelty: Novelty<S>,
    pools: PoolRegion<S>,
}

impl<S> SpectralBeats<S>
where
    S: HasPool<f32>,
{
    /// # Errors
    /// [`PoolError`] when the analysis window does not fit the region.
    pub fn new(pools: PoolRegion<S>) -> Result<Self, PoolError> {
        Ok(Self {
            novelty: Novelty::new(pools.clone())?,
            pools,
        })
    }

    /// Input: whole-track mono f32 at `22_050` Hz. Output: seconds. Audio
    /// too short to measure a periodicity over yields no marks.
    ///
    /// # Errors
    /// [`PoolError`] when a stage does not fit the region.
    pub fn analyze(&self, mono_22050: &[f32]) -> Result<RawBeats, PoolError> {
        let curve = self.novelty.curve(mono_22050)?;
        let periods = period::periods(&curve, &self.pools)?;
        let mean = mean(&curve);
        let beats = decode::beats(&curve, &periods, &self.pools)?
            .iter()
            .copied()
            .map(|at| BeatMark {
                at: frames::seconds(at),
                confidence: confidence(&curve, at, mean),
            })
            .collect();
        Ok(RawBeats {
            beats,
            downbeats: Vec::new(),
        })
    }
}

fn mean(curve: &[f32]) -> f32 {
    if curve.is_empty() {
        return 0.0;
    }
    curve.iter().sum::<f32>() / curve.len().to_f32().unwrap_or(1.0)
}

fn confidence(curve: &[f32], at: f32, mean: f32) -> f32 {
    if mean <= 0.0 {
        return 0.5;
    }
    let value = at
        .round()
        .to_usize()
        .and_then(|index| curve.get(index))
        .copied()
        .unwrap_or(0.0);
    (1.0 / (1.0 + (-(value - mean) / mean).exp())).clamp(CONFIDENCE_BOUNDS.0, CONFIDENCE_BOUNDS.1)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{dsp::clicks, test_pools::pools};

    fn tracker() -> SpectralBeats<impl HasPool<f32>> {
        SpectralBeats::new(pools()).expect("a fresh region has room for the window")
    }

    const SECONDS: f32 = 20.0;

    fn tempo_change(switch_seconds: f32, total_seconds: f32, first: f32, second: f32) -> Vec<f32> {
        let mut pcm = clicks::track(switch_seconds, first);
        pcm.extend(clicks::track(total_seconds - switch_seconds, second));
        pcm
    }

    fn detect(period_seconds: f32) -> Vec<f32> {
        let pcm = clicks::track(SECONDS, period_seconds);
        tracker()
            .analyze(&pcm)
            .expect("the analysis fits the region")
            .beats
            .into_iter()
            .map(|mark| mark.at)
            .collect()
    }

    #[kithara::test(native, flash(false))]
    fn markers_land_on_the_clicks() {
        let period = 0.5;
        let found = detect(period);
        let expected = clicks::positions(SECONDS, period);
        assert!(
            found.len() >= expected.len() - 1,
            "a marker per click: {} found for {} clicks",
            found.len(),
            expected.len()
        );
        for at in &found {
            let nearest = expected
                .iter()
                .map(|click| (click - at).abs())
                .fold(f32::INFINITY, f32::min);
            assert!(
                nearest < 0.070,
                "marker at {at} s is {nearest} s from any click, not merely correctly spaced"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn a_tempo_change_lands_in_separate_segments() {
        let switch = 20.0;
        let pcm = tempo_change(switch, 40.0, 60.0 / 100.0, 60.0 / 140.0);
        let beats: Vec<f32> = tracker()
            .analyze(&pcm)
            .expect("the analysis fits the region")
            .beats
            .into_iter()
            .map(|mark| mark.at)
            .collect();

        // Read each half away from the seam, where a window straddles both.
        let tempo = |lo: f32, hi: f32| {
            let mut gaps: Vec<f32> = beats
                .windows(2)
                .filter(|pair| pair[0] >= lo && pair[1] < hi)
                .map(|pair| pair[1] - pair[0])
                .collect();
            gaps.sort_by(f32::total_cmp);
            60.0 / gaps[gaps.len() / 2]
        };

        let before = tempo(4.0, switch - 4.0);
        let after = tempo(switch + 8.0, 38.0);
        assert!(
            (before - 100.0).abs() < 4.0,
            "the first segment keeps its own tempo, read as {before} BPM"
        );
        assert!(
            (after - 140.0).abs() < 4.0,
            "the second segment takes the new tempo, read as {after} BPM"
        );
    }

    /// A reused tracker carries pooled buffers between calls, so the same
    /// audio must decode to the same grid however it is reached.
    #[kithara::test(native, flash(false))]
    fn the_same_audio_yields_the_same_marks() {
        let pcm = tempo_change(9.0, 24.0, 60.0 / 100.0, 60.0 / 137.0);
        let pool = SamplePool::default();

        let mut reused = SpectralBeats::new(&pool);
        let first: Vec<f32> = reused.analyze(&pcm).beats.iter().map(|m| m.at).collect();
        let _ = reused.analyze(&clicks::track(12.0, 0.4));
        let again: Vec<f32> = reused.analyze(&pcm).beats.iter().map(|m| m.at).collect();
        let fresh: Vec<f32> = SpectralBeats::new(&pool)
            .analyze(&pcm)
            .beats
            .iter()
            .map(|m| m.at)
            .collect();

        assert_eq!(
            first, again,
            "a reused tracker carried state between tracks"
        );
        assert_eq!(first, fresh, "a fresh tracker disagreed with a reused one");
    }

    #[kithara::test(native, flash(false))]
    fn silence_yields_no_markers() {
        let raw = tracker()
            .analyze(&clicks::silence(20.0))
            .expect("the analysis fits the region");
        assert!(raw.beats.is_empty(), "silence has no beats to report");
        assert!(raw.downbeats.is_empty());
    }

    #[kithara::test(native, flash(false))]
    fn a_grid_carries_beats_and_no_downbeats() {
        let raw = tracker()
            .analyze(&clicks::track(SECONDS, 0.5))
            .expect("the analysis fits the region");
        assert!(!raw.beats.is_empty());
        assert!(
            raw.downbeats.is_empty(),
            "this tracker does not establish bar starts and reports none"
        );
        assert!(
            raw.beats
                .iter()
                .all(|mark| mark.confidence > 0.0 && mark.confidence < 1.0),
            "every mark carries a probability, never a certainty"
        );
    }
}

#[cfg(test)]
mod optimality {
    use kithara_bufpool::SampleBuffer;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        dsp::{clicks, decode, period},
        test_pools::pools,
    };

    /// Maximised over the starting state: the initial distribution is uniform.
    fn score(
        beats: &[usize],
        hazards: &[SampleBuffer],
        observations: &[f32],
        states: usize,
    ) -> f64 {
        let first = beats.first().copied().unwrap_or(0);
        (0..states)
            .filter(|start| start + first < states)
            .map(|start| {
                let mut total = 0.0f64;
                let mut state = start;
                for frame in 0..observations.len() {
                    if frame > 0 {
                        let hazard = &hazards[decode::estimate_of(frame, hazards.len())];
                        if beats.binary_search(&frame).is_ok() {
                            total += f64::from(hazard[state].max(1e-6).ln());
                            state = 0;
                        } else if state + 1 < states {
                            total += f64::from((1.0 - hazard[state]).max(1e-6).ln());
                            state += 1;
                        } else {
                            return f64::NEG_INFINITY;
                        }
                    }
                    total += f64::from(if state == 0 {
                        observations[frame].ln()
                    } else {
                        (1.0 - observations[frame]).ln()
                    });
                }
                total
            })
            .fold(f64::NEG_INFINITY, f64::max)
    }

    #[kithara::test(native, flash(false))]
    fn the_decoded_path_scores_the_dynamic_programs_optimum() {
        let region = pools();
        let pcm = clicks::track(20.0, 0.5);
        let curve = Novelty::new(region.clone())
            .expect("a fresh region has room for the window")
            .curve(&pcm)
            .expect("the curve fits the region");
        let periods = period::periods(&curve, &region).expect("the estimates fit the region");
        let (decoded, optimum, hazards, observations, states) =
            decode::probe(&curve, &periods, &region).expect("the probe fits the region");

        let beats: Vec<usize> = decoded
            .iter()
            .map(|frame| frame.round().to_usize().unwrap_or(0))
            .collect();
        assert!(!beats.is_empty(), "a click track decodes to beats");

        let scored = score(&beats, &hazards, &observations, states);
        assert!(
            (scored - f64::from(optimum)).abs() < 1.0,
            "decoded path scores {scored}, the dynamic program reported {optimum}"
        );
    }
}
