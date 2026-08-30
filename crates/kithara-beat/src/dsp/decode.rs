use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use num_traits::cast::ToPrimitive;

use super::{
    frames,
    period::{ACF_FRAME, ACF_STEP},
};
struct Consts;

impl Consts {
    /// Keeps a log-likelihood finite where the curve touches its own extremes.
    const EPSILON: f32 = 1e-6;
    /// The Gaussian's 99% support, as a multiple of the standard deviation.
    const SUPPORT: f32 = 2.58;
}

/// Beat positions in detection-function frames, decoded by Viterbi over a
/// first-order hidden Markov model whose state is the frames elapsed since the
/// last beat.
///
/// State 0 is the beat state; the only transitions out of a state are to the
/// next non-beat state or back to the beat. The observation likelihoods are
/// first-order polynomials in the normalised curve, and every non-beat state
/// shares one distribution.
pub(crate) fn beats<S>(
    curve: &[f32],
    periods: &[f32],
    pools: &PoolRegion<S>,
) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    decode(curve, periods, pools).map(|(beats, _)| beats)
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

fn decode<S>(
    curve: &[f32],
    periods: &[f32],
    pools: &PoolRegion<S>,
) -> Result<(SampleBuffer, f32), PoolError>
where
    S: HasPool<f32>,
{
    let Some(longest) = periods
        .iter()
        .copied()
        .filter(|period| *period > 0.0)
        .max_by(f32::total_cmp)
    else {
        return Ok((pools.get::<f32>(), f32::NEG_INFINITY));
    };
    let states = (longest + Consts::SUPPORT * frames::sigma())
        .ceil()
        .to_usize()
        .unwrap_or(0)
        + 1;
    if states < 2 || curve.is_empty() {
        return Ok((pools.get::<f32>(), f32::NEG_INFINITY));
    }

    let observation = normalise(curve, pools)?;
    let hazards: Vec<SampleBuffer> = periods
        .iter()
        .map(|&p| hazard(p, states, pools))
        .collect::<Result<_, _>>()?;
    let mut previous = collected(
        pools,
        (0..states).map(|state| log_observation(observation[0], state)),
    )?;
    let mut current = pools.get_with_len::<f32>(states)?;
    current.fill(f32::NEG_INFINITY);
    let mut back = vec![0u16; curve.len() * states];

    for frame in 1..curve.len() {
        let hazard = &hazards[estimate_for(frame, hazards.len())];
        let (from, score) = previous
            .iter()
            .enumerate()
            .map(|(state, value)| (state, value + hazard[state].max(Consts::EPSILON).ln()))
            .fold((0usize, f32::NEG_INFINITY), |best, next| {
                if next.1 > best.1 { next } else { best }
            });
        current[0] = score + log_observation(observation[frame], 0);
        back[frame * states] = u16::try_from(from).unwrap_or(0);
        for state in 1..states {
            let stay = (1.0 - hazard[state - 1]).max(Consts::EPSILON).ln();
            current[state] =
                previous[state - 1] + stay + log_observation(observation[frame], state);
            back[frame * states + state] = u16::try_from(state - 1).unwrap_or(0);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    let optimum = previous.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    Ok((
        backtrack(&previous, &back, states, curve.len(), pools)?,
        optimum,
    ))
}

#[cfg(test)]
pub(super) fn probe<S>(
    curve: &[f32],
    periods: &[f32],
    pools: &PoolRegion<S>,
) -> Result<(SampleBuffer, f32, Vec<SampleBuffer>, SampleBuffer, usize), PoolError>
where
    S: HasPool<f32>,
{
    let (beats, optimum) = decode(curve, periods, pools)?;
    let longest = periods
        .iter()
        .copied()
        .filter(|period| *period > 0.0)
        .max_by(f32::total_cmp)
        .unwrap_or(0.0);
    let states = (longest + Consts::SUPPORT * frames::sigma())
        .ceil()
        .to_usize()
        .unwrap_or(0)
        + 1;
    let hazards = periods
        .iter()
        .map(|&p| hazard(p, states, pools))
        .collect::<Result<_, _>>()?;
    Ok((
        beats,
        optimum,
        hazards,
        normalise(curve, pools)?,
        states,
    ))
}

#[cfg(test)]
pub(super) fn estimate_of(frame: usize, estimates: usize) -> usize {
    estimate_for(frame, estimates)
}

/// An estimate describes the window it was measured over, so the frame it
/// applies to is that window's centre rather than its start.
fn estimate_for(frame: usize, estimates: usize) -> usize {
    frame
        .saturating_sub(ACF_FRAME / 2)
        .div_euclid(ACF_STEP)
        .min(estimates - 1)
}

fn backtrack<S>(
    last: &[f32],
    back: &[u16],
    states: usize,
    frames: usize,
    pools: &PoolRegion<S>,
) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    let mut state = last
        .iter()
        .enumerate()
        .fold((0usize, f32::NEG_INFINITY), |best, (index, &value)| {
            if value > best.1 { (index, value) } else { best }
        })
        .0;
    // Walked backwards, so the beats are written front to back and turned
    // round once the count is known.
    let mut out = pools.get_with_len::<f32>(frames)?;
    let mut found = 0;
    for frame in (0..frames).rev() {
        if state == 0 {
            out[found] = frame.to_f32().unwrap_or(0.0);
            found += 1;
        }
        state = back[frame * states + state].into();
    }
    out.truncate(found);
    out.reverse();
    Ok(out)
}

/// The curve read as a probability: the beat state is likelier where the curve
/// is large, every non-beat state where it is small.
fn normalise<S>(curve: &[f32], pools: &PoolRegion<S>) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    let peak = curve.iter().copied().fold(0.0f32, f32::max);
    if peak <= 0.0 {
        return pools.get_with_len::<f32>(curve.len());
    }
    collected(
        pools,
        curve
            .iter()
            .map(|value| (value / peak).clamp(Consts::EPSILON, 1.0 - Consts::EPSILON)),
    )
}

fn log_observation(value: f32, state: usize) -> f32 {
    if state == 0 {
        value.ln()
    } else {
        (1.0 - value).ln()
    }
}

/// Probability of leaving each state for the beat state, from a Gaussian over
/// the time between consecutive beats centred on the estimated period.
fn hazard<S>(period: f32, states: usize, pools: &PoolRegion<S>) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    if period <= 0.0 {
        return pools.get_with_len::<f32>(states);
    }
    let sigma = frames::sigma();
    let variance = 2.0 * sigma * sigma;
    let interval = collected(
        pools,
        (0..states).map(|state| {
            let gap = (state + 1).to_f32().unwrap_or(0.0) - period;
            (-gap * gap / variance).exp()
        }),
    )?;
    let mut remaining: f32 = interval.iter().sum();
    collected(
        pools,
        interval.iter().map(|&density| {
        let leave = if remaining > 0.0 {
            density / remaining
        } else {
            1.0
        };
            remaining -= density;
            leave.clamp(0.0, 1.0)
        }),
    )
}
