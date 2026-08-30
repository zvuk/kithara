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
    /// Resolution the period is searched at. A beat period is rarely a whole
    /// number of frames, and sampling the correlation between lags is what
    /// lets a hypothesis be scored at its own multiples rather than at the
    /// nearest integers to them.
    const STEP: f32 = 0.25;
    /// Hypotheses spanning `MIN_LAG..=MAX_LAG` at [`Consts::STEP`].
    const HYPOTHESES: usize = (Self::MAX_LAG - Self::MIN_LAG) * 4 + 1;
}

fn period_of(hypothesis: usize) -> f32 {
    Consts::MIN_LAG.to_f32().unwrap_or(0.0) + hypothesis.to_f32().unwrap_or(0.0) * Consts::STEP
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
    let mut saliences: Vec<SampleBuffer> = Vec::new();
    for start in (0..=curve.len() - ACF_FRAME).step_by(ACF_STEP) {
        let mut autocorrelation = zeroed(pools, ACF_FRAME)?;
        correlate(&curve[start..start + ACF_FRAME], &mut autocorrelation, pools)?;
        saliences.push(salience(&autocorrelation, pools)?);
    }
    collected(
        pools,
        track(&saliences, pools)?
            .into_iter()
            .map(|hypothesis| hypothesis.map_or(0.0, period_of)),
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
    let lags = Consts::HYPOTHESES;
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
                let period = period_of(from);
                let spread = sigma * (gap_frames / period).sqrt();
                let gap =
                    (index.to_f32().unwrap_or(0.0) - from.to_f32().unwrap_or(0.0)) * Consts::STEP;
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
        out[step] = saliences[step]
            .iter()
            .any(|&value| value > 0.0)
            .then_some(index);
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
        *slot = rectified[lag..]
            .iter()
            .zip(rectified.iter())
            .map(|(a, b)| a * b)
            .sum();
    }
    Ok(())
}

/// Shift-invariant comb filterbank under the tempo preference curve. All zero
/// when the window carries no onset energy: silence has no period.
fn salience<S>(autocorrelation: &[f32], pools: &PoolRegion<S>) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    if autocorrelation.first().is_none_or(|&energy| energy <= 0.0) {
        return zeroed(pools, Consts::HYPOTHESES);
    }
    let mut out = zeroed(pools, Consts::HYPOTHESES)?;
    for (hypothesis, slot) in out.iter_mut().enumerate() {
        let period = period_of(hypothesis);
        let mut score = 0.0;
        for harmonic in 1..=Consts::COMB_HARMONICS {
            let weight = (2 * harmonic - 1).to_f32().unwrap_or(1.0);
            let at = period * harmonic.to_f32().unwrap_or(1.0);
            score += sample(autocorrelation, at) / weight;
        }
        *slot = score * rayleigh(period);
    }
    Ok(out)
}

/// The correlation between its samples, so a hypothesis is read at its own
/// multiples instead of at the whole lags nearest them.
fn sample(autocorrelation: &[f32], at: f32) -> f32 {
    let below = at.floor();
    let Some(index) = below.to_usize() else {
        return 0.0;
    };
    let here = autocorrelation.get(index).copied().unwrap_or(0.0);
    let next = autocorrelation.get(index + 1).copied().unwrap_or(0.0);
    here + (next - here) * (at - below)
}

fn rayleigh(period: f32) -> f32 {
    let variance = Consts::RAYLEIGH_B * Consts::RAYLEIGH_B;
    period / variance * (-period * period / (2.0 * variance)).exp()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        dsp::{clicks, novelty::Novelty},
        test_pools::pools,
    };

    fn curve_of(pcm: &[f32], region: &PoolRegion<impl HasPool<f32>>) -> SampleBuffer {
        Novelty::new(region.clone())
            .expect("a fresh region has room for the window")
            .curve(pcm)
            .expect("the curve fits the region")
    }

    fn bpm(lag: f32) -> f32 {
        60.0 / (lag * frames::frame_seconds())
    }

    fn track_bpm(beats_per_minute: f32) -> SampleBuffer {
        let region = pools();
        let pcm = clicks::track(20.0, 60.0 / beats_per_minute);
        let curve = curve_of(&pcm, &region);
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

    /// The decoder sizes its state space from the longest period it is given
    /// and allocates against it, so a period from outside the searched range
    /// is not a wrong answer but an unbounded allocation.
    #[kithara::test(native, flash(false))]
    fn every_period_stays_inside_the_searched_range() {
        let region = pools();
        let curve = curve_of(&clicks::track(20.0, 0.5), &region);
        let reported = periods(&curve, &region).expect("the estimates fit the region");
        assert!(!reported.is_empty(), "20 s of audio yields estimates");

        let range = period_of(0)..=period_of(Consts::HYPOTHESES - 1);
        for &period in reported.iter() {
            assert!(
                period == 0.0 || range.contains(&period),
                "period {period} left the searched range {range:?}"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn silence_has_no_period_to_report() {
        let region = pools();
        let curve = curve_of(&clicks::silence(0.5), &region);
        assert!(
            periods(&curve, &region)
                .expect("the estimates fit the region")
                .is_empty(),
            "audio shorter than a periodicity window yields nothing"
        );
    }
}
