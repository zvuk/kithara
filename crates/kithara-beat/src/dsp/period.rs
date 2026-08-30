use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use num_traits::cast::ToPrimitive;

use super::frames;

/// Periodicity window, 512 detection-function frames (5.94 s).
pub(crate) const ACF_FRAME: usize = 512;
/// One beat-period estimate every 128 frames (1.49 s), a 75% overlap.
pub(crate) const ACF_STEP: usize = 128;
struct Consts;

impl Consts {
    /// Comb elements each hypothesis is scored over.
    const COMB_HARMONICS: usize = 4;
    /// 40 BPM, the slowest period tracked.
    const MAX_LAG: usize = 129;
    /// 208 BPM, the fastest.
    const MIN_LAG: usize = 25;
    /// Rayleigh mode, in lags: the mean beat period of the source paper's
    /// database.
    const RAYLEIGH_B: f32 = 48.0;
}

/// Beat period in detection-function frames, one per [`ACF_STEP`].
///
/// Fractional: a beat period is rarely a whole number of frames, and at these
/// tempi one lag is worth about 2.4 BPM, which a grid cannot absorb.
///
/// Empty when the curve is shorter than one periodicity window: there is no
/// periodicity to measure over less than 6 seconds.
pub(crate) fn periods<S>(curve: &[f32], pools: &PoolRegion<S>) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    if curve.len() < ACF_FRAME {
        return Ok(pools.get::<f32>());
    }
    let weights = rayleigh(pools)?;
    let mut correlations: Vec<SampleBuffer> = Vec::new();
    let mut saliences: Vec<SampleBuffer> = Vec::new();
    for start in (0..=curve.len() - ACF_FRAME).step_by(ACF_STEP) {
        let mut autocorrelation = zeroed(pools, ACF_FRAME)?;
        correlate(&curve[start..start + ACF_FRAME], &mut autocorrelation, pools)?;
        saliences.push(salience(&autocorrelation, &weights, pools)?);
        correlations.push(autocorrelation);
    }
    collected(
        pools,
        track(&saliences, pools)?
            .into_iter()
            .zip(correlations.iter())
            .map(|(lag, autocorrelation)| lag.map_or(0.0, |lag| interpolate(autocorrelation, lag))),
    )
}

fn zeroed<S>(pools: &PoolRegion<S>, len: usize) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    pools.get_with_len::<f32>(len)
}

/// Fills a pooled buffer from `values` in the order they arrive.
fn collected<S, I>(pools: &PoolRegion<S>, values: I) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
    I: IntoIterator<Item = f32>,
{
    let staged: Vec<f32> = values.into_iter().collect();
    let mut out = pools.get::<f32>();
    out.try_extend_from_slice(&staged)?;
    Ok(out)
}

/// The beat period is a slowly varying process, so the sequence of estimates is
/// decoded rather than read off each window's own maximum: a window whose
/// salience momentarily favours another metrical level does not drag the grid
/// with it.
///
/// The transition is Gaussian over lag. Its width is not the beat-to-beat
/// deviation but that deviation accumulated over the beats separating two
/// estimates: timing wanders as a random walk, so over `n` beats it spreads by
/// `sqrt(n)`. Reading the phase model's tolerance as the per-beat step makes
/// this the same constant rather than a second, invented one.
fn track<S>(saliences: &[SampleBuffer], pools: &PoolRegion<S>) -> Result<Vec<Option<usize>>, PoolError>
where
    S: HasPool<f32>,
{
    let lags = Consts::MAX_LAG - Consts::MIN_LAG + 1;
    let gap_frames = ACF_STEP.to_f32().unwrap_or(1.0);
    let sigma = frames::sigma();
    let mut costs = collected(
        pools,
        saliences
            .first()
            .map(|row| row.iter().map(|&v| v.max(f32::MIN_POSITIVE).ln()))
            .into_iter()
            .flatten(),
    )?;
    let mut back = vec![0usize; saliences.len() * lags];

    for (step, row) in saliences.iter().enumerate().skip(1) {
        let mut next = zeroed(pools, lags)?;
        next.fill(f32::NEG_INFINITY);
        for (index, slot) in next.iter_mut().enumerate() {
            let mut best = (0usize, f32::NEG_INFINITY);
            for (from, cost) in costs.iter().enumerate() {
                let period = (Consts::MIN_LAG + from).to_f32().unwrap_or(1.0);
                let spread = sigma * (gap_frames / period).sqrt();
                let gap = index.to_f32().unwrap_or(0.0) - from.to_f32().unwrap_or(0.0);
                let score = cost - gap * gap / (2.0 * spread * spread);
                if score > best.1 {
                    best = (from, score);
                }
            }
            *slot = best.1 + row[index].max(f32::MIN_POSITIVE).ln();
            back[step * lags + index] = best.0;
        }
        costs = next;
    }

    let mut index = costs
        .iter()
        .enumerate()
        .fold((0usize, f32::NEG_INFINITY), |best, (i, &v)| {
            if v > best.1 { (i, v) } else { best }
        })
        .0;
    let mut out = vec![None; saliences.len()];
    for step in (0..saliences.len()).rev() {
        out[step] = if saliences[step].iter().any(|&v| v > 0.0) {
            Some(Consts::MIN_LAG + index)
        } else {
            None
        };
        index = back[step * lags + index];
    }
    Ok(out)
}

/// Unbiased autocorrelation of the frame, mean-thresholded and half-wave
/// rectified first so a loud passage does not outweigh a quiet one.
fn correlate<S>(frame: &[f32], out: &mut [f32], pools: &PoolRegion<S>) -> Result<(), PoolError>
where
    S: HasPool<f32>,
{
    let count = frame.len().to_f32().unwrap_or(1.0);
    let mean = frame.iter().sum::<f32>() / count;
    let rectified = collected(pools, frame.iter().map(|&v| (v - mean).max(0.0)))?;
    for (lag, slot) in out.iter_mut().enumerate() {
        let pairs = rectified.len() - lag;
        let sum: f32 = rectified[lag..]
            .iter()
            .zip(rectified.iter())
            .map(|(a, b)| a * b)
            .sum();
        *slot = sum / pairs.to_f32().unwrap_or(1.0);
    }
    Ok(())
}

/// Shift-invariant comb filterbank under the tempo preference curve: each lag
/// scores the autocorrelation at its own multiples, so a period beats its own
/// submultiples only when the harmonics agree.
///
/// All zero when the window carries no onset energy: silence has no period, and
/// reporting one would put a grid on nothing.
fn salience<S>(
    autocorrelation: &[f32],
    weights: &[f32],
    pools: &PoolRegion<S>,
) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    if autocorrelation.first().is_none_or(|&energy| energy <= 0.0) {
        return zeroed(pools, Consts::MAX_LAG - Consts::MIN_LAG + 1);
    }
    let mut out = zeroed(pools, Consts::MAX_LAG - Consts::MIN_LAG + 1)?;
    for (lag, weight) in weights.iter().enumerate().skip(Consts::MIN_LAG) {
        let mut score = 0.0;
        for harmonic in 1..=Consts::COMB_HARMONICS {
            // An integer lag stands for periods in `[lag, lag + 1]`, so its
            // `harmonic`th multiple falls anywhere across that many lags.
            let span = harmonic + 1;
            let first = harmonic * lag;
            let sum: f32 = (first..=first + harmonic)
                .filter_map(|offset| autocorrelation.get(offset))
                .sum();
            score += sum / span.to_f32().unwrap_or(1.0);
        }
        out[lag - Consts::MIN_LAG] = score * weight;
    }
    Ok(out)
}

/// Sub-lag period by fitting a parabola through the autocorrelation peak the
/// winning lag stands on. The filterbank picks the metrical level; this reads
/// the period off the correlation itself, whose peak is symmetric where the
/// filterbank's score is not.
fn interpolate(autocorrelation: &[f32], lag: usize) -> f32 {
    let peak = [lag, lag + 1]
        .into_iter()
        .max_by(|&a, &b| {
            autocorrelation
                .get(a)
                .unwrap_or(&0.0)
                .total_cmp(autocorrelation.get(b).unwrap_or(&0.0))
        })
        .unwrap_or(lag);
    let centre = peak.to_f32().unwrap_or(0.0);
    let (Some(&before), Some(&at), Some(&after)) = (
        peak.checked_sub(1).and_then(|i| autocorrelation.get(i)),
        autocorrelation.get(peak),
        autocorrelation.get(peak + 1),
    ) else {
        return centre;
    };
    let curvature = before - 2.0 * at + after;
    if curvature >= 0.0 {
        return centre;
    }
    centre + 0.5 * (before - after) / curvature
}

fn rayleigh<S>(pools: &PoolRegion<S>) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    let variance = Consts::RAYLEIGH_B * Consts::RAYLEIGH_B;
    collected(
        pools,
        (0..=Consts::MAX_LAG).map(|lag| {
            let l = lag.to_f32().unwrap_or(0.0);
            l / variance * (-l * l / (2.0 * variance)).exp()
        }),
    )
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        dsp::{clicks, novelty::Novelty},
        test_pools::pools,
    };

    fn bpm(lag: f32) -> f32 {
        60.0 / (lag * frames::frame_seconds())
    }

    fn track_bpm(beats_per_minute: f32) -> SampleBuffer {
        let region = pools();
        let pcm = clicks::track(20.0, 60.0 / beats_per_minute);
        let curve = Novelty::new(region.clone())
            .expect("a fresh region has room for the window")
            .curve(&pcm)
            .expect("the curve fits the region");
        periods(&curve, &region).expect("the estimates fit the region")
    }

    #[kithara::test(native, flash(false))]
    fn a_click_track_yields_its_own_tempo() {
        for want in [90.0, 120.0, 150.0] {
            let estimates = track_bpm(want);
            assert!(!estimates.is_empty(), "20 s of audio yields estimates");
            for &lag in estimates.iter() {
                let got = bpm(lag);
                assert!(
                    (got - want).abs() < 2.0,
                    "click track at {want} BPM estimated as {got} BPM"
                );
            }
        }
    }

    #[kithara::test(native, flash(false))]
    fn half_and_double_tempo_are_distinguished() {
        let slow = track_bpm(75.0);
        let fast = track_bpm(150.0);
        let slow_lag = slow[0];
        let fast_lag = fast[0];
        assert!(
            (bpm(slow_lag) - 75.0).abs() < 2.0 && (bpm(fast_lag) - 150.0).abs() < 2.0,
            "75 BPM read as {} and 150 BPM as {}: the two levels must not collapse",
            bpm(slow_lag),
            bpm(fast_lag)
        );
    }

    #[kithara::test(native, flash(false))]
    fn silence_has_no_period_to_report() {
        let region = pools();
        let curve = Novelty::new(region.clone())
            .expect("a fresh region has room for the window")
            .curve(&clicks::silence(0.5))
            .expect("the curve fits the region");
        assert!(
            periods(&curve, &region)
                .expect("the estimates fit the region")
                .is_empty(),
            "audio shorter than a periodicity window yields nothing"
        );
    }
}
