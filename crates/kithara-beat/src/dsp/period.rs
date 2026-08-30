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

/// Fractional beat period in frames, one per [`ACF_STEP`]. Empty when the
/// curve is shorter than one periodicity window.
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

/// Viterbi over the salience: the period is a slowly varying process. The
/// transition width is the per-beat deviation spread over the beats between
/// two estimates, timing being a random walk.
fn track<S>(
    saliences: &[SampleBuffer],
    pools: &PoolRegion<S>,
) -> Result<Vec<Option<usize>>, PoolError>
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

/// Shift-invariant comb filterbank under the tempo preference curve. All zero
/// when the window carries no onset energy: silence has no period.
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
    // Only a sample that tops its neighbours has a parabola to refine, and
    // that bound keeps the vertex inside the half-lag it stands for. A ramp
    // with a whisker of concavity otherwise puts it arbitrarily far away.
    if at < before || at < after {
        return centre;
    }
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

    /// Sub-bin interpolation may only move a peak inside its own bin. A nearly
    /// flat peak makes the parabola's vertex run away, and the result becomes a
    /// period no filterbank ever proposed.
    #[kithara::test(native, flash(false))]
    fn interpolation_stays_inside_the_searched_range() {
        let mut autocorrelation = vec![0.0; ACF_FRAME];
        // An upward ramp with a whisker of concavity: the parabola's vertex
        // sits nowhere near the three lags that defined it.
        autocorrelation[50] = 1.0;
        autocorrelation[51] = 1.1;
        autocorrelation[52] = 1.199_999_9;

        let period = interpolate(&autocorrelation, 50);
        assert!(
            (Consts::MIN_LAG.to_f32().unwrap_or(0.0)..=Consts::MAX_LAG.to_f32().unwrap_or(0.0))
                .contains(&period),
            "interpolated period {period} left the searched lag range"
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
