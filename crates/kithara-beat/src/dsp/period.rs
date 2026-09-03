use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use num_traits::cast::ToPrimitive;

/// Periodicity window, 512 detection-function frames (5.94 s).
pub(crate) const ACF_FRAME: usize = 512;
/// One beat-period estimate every 128 frames (1.49 s), a 75% overlap.
pub(crate) const ACF_STEP: usize = 128;
struct Consts;

impl Consts {
    /// Tracked-period band, 28..=108 lags (47.9..184.6 BPM): estimates only
    /// move inside it, so a hypothesis outside can win a single frame but
    /// never the path.
    const BAND: std::ops::RangeInclusive<usize> = 27..=107;
    /// Comb elements each hypothesis is scored over.
    const COMB_HARMONICS: usize = 4;
    /// Hypothesis `i` is a period of `i + 1` lags; one per possible lag up
    /// to the estimate spacing.
    const HYPOTHESES: usize = ACF_STEP;
    /// Widest comb element reaches 3 lags below its harmonic, and the top
    /// hypothesis is where its widest element still reads inside the window.
    const PERIOD_INDEX: std::ops::RangeInclusive<usize> = (Self::COMB_HARMONICS - 1)
        ..=((ACF_FRAME - (Self::COMB_HARMONICS - 1)) / Self::COMB_HARMONICS - 1);
    /// Rayleigh mode in lags: a 120 BPM preference at the detection rate.
    const RAYLEIGH_LAG: f32 = 43.0;
    /// Adaptive-threshold half window, 0.1 s of detection frames.
    const SMOOTH_HALF: usize = 8;
    /// Between-estimate spread of the period, in lags.
    const TRANSITION_SIGMA: f32 = 8.0;
    /// The Gaussian transition's support, in lags.
    const TRANSITION_SUPPORT: f32 = 32.0;
    /// Hypotheses below 25 lags sit above 208 BPM and are outside the
    /// searched tempo range.
    const SEARCH_MIN_INDEX: usize = 24;
}

/// Fills a pooled buffer of `len` from `values`, in the order they arrive.
fn collected<S, I>(pools: &PoolRegion<S>, len: usize, values: I) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
    I: IntoIterator<Item = f32>,
{
    let mut out = pools.get_with_len::<f32>(len)?;
    for (slot, value) in out.iter_mut().zip(values) {
        *slot = value;
    }
    Ok(out)
}

fn lag_of(hypothesis: usize) -> f32 {
    (hypothesis + 1).to_f32().unwrap_or(0.0)
}

/// Beat period in whole frames, one per [`ACF_STEP`]. Empty when the curve
/// is shorter than one periodicity window.
pub(crate) fn periods<S>(curve: &[f32], pools: &PoolRegion<S>) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    if curve.len() < ACF_FRAME {
        return Ok(pools.get::<f32>());
    }
    let mut onsets = collected(pools, curve.len(), curve.iter().copied())?;
    adaptive_threshold(&mut onsets, Consts::SMOOTH_HALF, pools)?;

    let weights = rayleigh_weights(pools)?;
    let mut window = zeroed(pools, ACF_FRAME)?;
    let mut autocorrelation = zeroed(pools, ACF_FRAME)?;
    let mut saliences: Vec<SampleBuffer> = Vec::new();
    // The last window is filled out with zeros, and the first window that
    // needs that is the last.
    let mut start = 0;
    loop {
        let end = (start + ACF_FRAME).min(onsets.len());
        window.fill(0.0);
        window[..end - start].copy_from_slice(&onsets[start..end]);
        correlate(&window, &mut autocorrelation);
        let mut salience = comb(&autocorrelation, &weights, pools)?;
        adaptive_threshold(&mut salience, Consts::SMOOTH_HALF, pools)?;
        salience[..Consts::SEARCH_MIN_INDEX].fill(0.0);
        saliences.push(salience);
        if start + ACF_FRAME > onsets.len() {
            break;
        }
        start += ACF_STEP;
    }
    let hypotheses = track(&saliences, &weights, pools)?;
    collected(
        pools,
        hypotheses.len(),
        hypotheses
            .into_iter()
            .map(|hypothesis| hypothesis.map_or(0.0, lag_of)),
    )
}

fn zeroed<S>(pools: &PoolRegion<S>, len: usize) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    pools.get_with_len::<f32>(len)
}

/// Subtract a centred moving average, edges replicated, and half-wave
/// rectify: the strongest peaks survive, slow trends do not.
fn adaptive_threshold<S>(
    values: &mut [f32],
    half: usize,
    pools: &PoolRegion<S>,
) -> Result<(), PoolError>
where
    S: HasPool<f32>,
{
    if values.is_empty() {
        return Ok(());
    }
    let last = values.len() - 1;
    let count = (2 * half + 1).to_f32().unwrap_or(1.0);
    let smoothed = collected(
        pools,
        values.len(),
        (0..values.len()).map(|at| {
            (at.saturating_sub(half)..=(at + half).min(last))
                .map(|index| values[index])
                .sum::<f32>()
                + values[0] * half.saturating_sub(at).to_f32().unwrap_or(0.0)
                + values[last] * (at + half).saturating_sub(last).to_f32().unwrap_or(0.0)
        }),
    )?;
    for (value, mean) in values.iter_mut().zip(smoothed.iter()) {
        *value = (*value - mean / count).max(0.0);
    }
    Ok(())
}

/// Viterbi over the salience under the Gaussian transition: the period is a
/// slowly varying process, and the tempo preference is the prior.
fn track<S>(
    saliences: &[SampleBuffer],
    weights: &[f32],
    pools: &PoolRegion<S>,
) -> Result<Vec<Option<usize>>, PoolError>
where
    S: HasPool<f32>,
{
    let lags = Consts::HYPOTHESES;
    let variance = 2.0 * Consts::TRANSITION_SIGMA * Consts::TRANSITION_SIGMA;
    let mut costs = collected(
        pools,
        lags,
        saliences
            .first()
            .map(|row| {
                row.iter().zip(weights.iter()).map(|(&value, &weight)| {
                    value.max(f32::MIN_POSITIVE).ln() + weight.max(f32::MIN_POSITIVE).ln()
                })
            })
            .into_iter()
            .flatten(),
    )?;
    let mut back = vec![0usize; saliences.len() * lags];

    for (step, row) in saliences.iter().enumerate().skip(1) {
        let mut next = pools.get_with_len::<f32>(lags)?;
        next.fill(f32::NEG_INFINITY);
        for index in Consts::BAND {
            let mut best = (0usize, f32::NEG_INFINITY);
            for from in Consts::BAND {
                let gap = index.to_f32().unwrap_or(0.0) - from.to_f32().unwrap_or(0.0);
                if gap.abs() > Consts::TRANSITION_SUPPORT {
                    continue;
                }
                let score = costs[from] - gap * gap / variance;
                if score > best.1 {
                    best = (from, score);
                }
            }
            next[index] = best.1 + row[index].max(f32::MIN_POSITIVE).ln();
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
        out[step] = saliences[step]
            .iter()
            .any(|&value| value > 0.0)
            .then_some(index);
        index = back[step * lags + index];
    }
    Ok(out)
}

fn correlate(frame: &[f32], out: &mut [f32]) {
    let count = frame.len().to_f32().unwrap_or(1.0);
    for (lag, slot) in out.iter_mut().enumerate() {
        let sum: f32 = frame[lag..]
            .iter()
            .zip(frame.iter())
            .map(|(a, b)| a * b)
            .sum();
        *slot = sum / (count - lag.to_f32().unwrap_or(0.0));
    }
}

/// Comb filterbank under the tempo preference curve. Each element's width
/// grows with its harmonic and its height is normalised by that width,
/// absorbing the autocorrelation's coarser resolution at multiples.
fn comb<S>(
    autocorrelation: &[f32],
    weights: &[f32],
    pools: &PoolRegion<S>,
) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    let mut out = zeroed(pools, Consts::HYPOTHESES)?;
    for harmonic in 1..=Consts::COMB_HARMONICS {
        let width = (2 * harmonic - 1).to_f32().unwrap_or(1.0);
        for offset in 0..(2 * harmonic - 1) {
            for index in Consts::PERIOD_INDEX {
                let at = (index + 1) * harmonic - harmonic + offset;
                out[index] += weights[index] * autocorrelation[at] / width;
            }
        }
    }
    Ok(out)
}

fn rayleigh_weights<S>(pools: &PoolRegion<S>) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    let variance = Consts::RAYLEIGH_LAG * Consts::RAYLEIGH_LAG;
    collected(
        pools,
        Consts::HYPOTHESES,
        (0..Consts::HYPOTHESES).map(|index| {
            let lag = lag_of(index);
            lag / variance * (-lag * lag / (2.0 * variance)).exp()
        }),
    )
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        dsp::{clicks, frames, novelty::Novelty},
        test_pools::pools,
    };

    fn bpm(lag: f32) -> f32 {
        60.0 / (lag * frames::frame_seconds())
    }

    fn track_bpm(beats_per_minute: f32) -> SampleBuffer {
        let pools = pools();
        let pcm = clicks::track(20.0, 60.0 / beats_per_minute);
        let curve = Novelty::new(pools.clone())
            .expect("a fresh region has room for the window")
            .curve(&pcm)
            .expect("the curve fits the region");
        periods(&curve, &pools).expect("the estimates fit the region")
    }

    /// Estimates are whole lags: the lag nearest the true period and its
    /// neighbours are correct answers, another metrical level (14 lags away
    /// at the closest) is not. A trailing estimate whose window is mostly
    /// past the audio wobbles wider, and one that has run out of clicks
    /// reports no period at all, which is not a tempo reading.
    #[kithara::test(native, flash(false))]
    fn a_click_track_yields_its_own_tempo() {
        for want in [90.0, 120.0, 150.0] {
            let estimates = track_bpm(want);
            let mut reported: Vec<f32> =
                estimates.iter().copied().filter(|&lag| lag > 0.0).collect();
            assert!(
                reported.len() >= estimates.len() / 2,
                "20 s of clicks yields mostly real estimates"
            );
            let true_lag = 60.0 / (want * frames::frame_seconds());
            for &lag in &reported {
                assert!(
                    (lag - true_lag).abs() <= 4.0,
                    "click track at {want} BPM ({true_lag} lags) estimated as {} BPM ({lag} lags)",
                    bpm(lag),
                );
            }
            reported.sort_by(f32::total_cmp);
            let median = reported[reported.len() / 2];
            assert!(
                (median - true_lag).abs() <= 1.5,
                "click track at {want} BPM ({true_lag} lags) tracked at {} BPM ({median} lags)",
                bpm(median),
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn half_and_double_tempo_are_distinguished() {
        let slow = track_bpm(75.0);
        let fast = track_bpm(150.0);
        let slow_lag = slow[0];
        let fast_lag = fast[0];
        assert!(
            (bpm(slow_lag) - 75.0).abs() < 2.5 && (bpm(fast_lag) - 150.0).abs() < 2.5,
            "75 BPM read as {} and 150 BPM as {}: the two levels must not collapse",
            bpm(slow_lag),
            bpm(fast_lag)
        );
    }

    /// The decoder sizes its state space from the longest period it is given
    /// and allocates against it, so a period from outside the searched range
    /// is not a wrong answer but an unbounded allocation.
    #[kithara::test(native, flash(false))]
    fn every_period_stays_inside_the_searched_range() {
        let pools = pools();
        let curve = Novelty::new(pools.clone())
            .expect("a fresh region has room for the window")
            .curve(&clicks::track(20.0, 0.5))
            .expect("the curve fits the region");
        let reported = periods(&curve, &pools).expect("the estimates fit the region");
        assert!(!reported.is_empty(), "20 s of audio yields estimates");

        let range = 1.0..=lag_of(Consts::HYPOTHESES - 1);
        for &period in reported.iter() {
            assert!(
                period == 0.0 || range.contains(&period),
                "period {period} left the searched range {range:?}"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn silence_has_no_period_to_report() {
        let pools = pools();
        let curve = Novelty::new(pools.clone())
            .expect("a fresh region has room for the window")
            .curve(&clicks::silence(0.5))
            .expect("the curve fits the region");
        assert!(
            periods(&curve, &pools)
                .expect("the estimates fit the region")
                .is_empty(),
            "audio shorter than a periodicity window yields nothing"
        );
    }
}
