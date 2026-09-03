use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use num_traits::cast::ToPrimitive;

use super::{frames, period::ACF_STEP};
struct Consts;

impl Consts {
    /// Height scale of the interval density: the Gaussian claims about 0.43
    /// of each state's transition mass, keeping every beat transition soft.
    const DENSITY_SCALE: f32 = 0.005;
    const EPSILON: f32 = 1e-6;
    /// Observations top out below one, so a skipped peak stays payable.
    const OBSERVED_CEILING: f32 = 0.99;
    /// How far past the longest period the state space reaches, in standard
    /// deviations: the longest wait the decoder can express.
    const STATE_MARGIN: f32 = 3.0;
    /// The interval density's support, in standard deviations.
    const SUPPORT: f32 = 4.0;
}

/// Beat positions in frames, by Viterbi over a hidden Markov model whose
/// state counts the frames since the last beat. State 0 is the beat.
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
    let states = (longest + Consts::STATE_MARGIN * frames::sigma())
        .floor()
        .to_usize()
        .unwrap_or(0);
    if states < 2 || curve.is_empty() {
        return Ok((pools.get::<f32>(), f32::NEG_INFINITY));
    }

    let observation = normalise(curve, pools)?;
    let hazards: Vec<SampleBuffer> = periods
        .iter()
        .map(|&p| hazard(p, states, pools))
        .collect::<Result<_, _>>()?;
    // The path starts as if a beat fell just before the first frame.
    let mut previous = pools.get_with_len::<f32>(states)?;
    previous.fill(f32::NEG_INFINITY);
    if let Some(first) = previous.first_mut() {
        *first = 0.0;
    }
    let mut current = pools.get_with_len::<f32>(states)?;
    current.fill(f32::NEG_INFINITY);
    let mut back = vec![0u16; curve.len() * states];

    for frame in 0..curve.len() {
        let hazard = &hazards[estimate_for(frame, hazards.len())];
        let (from, score) = previous
            .iter()
            .enumerate()
            .map(|(state, value)| (state, value + hazard[state].ln()))
            .fold((0usize, f32::NEG_INFINITY), |best, next| {
                if next.1 > best.1 { next } else { best }
            });
        current[0] = score + log_observation(observation[frame], 0);
        back[frame * states] = u16::try_from(from).unwrap_or(0);
        for state in 1..states {
            let stay = (1.0 - hazard[state - 1]).ln();
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
    let states = (longest + Consts::STATE_MARGIN * frames::sigma())
        .floor()
        .to_usize()
        .unwrap_or(0);
    let hazards = periods
        .iter()
        .map(|&p| hazard(p, states, pools))
        .collect::<Result<_, _>>()?;
    Ok((beats, optimum, hazards, normalise(curve, pools)?, states))
}

#[cfg(test)]
pub(super) fn estimate_of(frame: usize, estimates: usize) -> usize {
    estimate_for(frame, estimates)
}

/// The estimate measured over the window opening at step `k` applies during
/// step `k + 1`.
fn estimate_for(frame: usize, estimates: usize) -> usize {
    (frame / ACF_STEP).saturating_sub(1).min(estimates - 1)
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

fn normalise<S>(curve: &[f32], pools: &PoolRegion<S>) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    let peak = curve.iter().copied().fold(0.0f32, f32::max);
    if peak <= 0.0 {
        let mut flat = pools.get_with_len::<f32>(curve.len())?;
        flat.fill(Consts::EPSILON);
        return Ok(flat);
    }
    collected(
        pools,
        curve.len(),
        curve
            .iter()
            .map(|value| (Consts::OBSERVED_CEILING * value / peak).max(Consts::EPSILON)),
    )
}

fn log_observation(value: f32, state: usize) -> f32 {
    if state == 0 {
        value.ln()
    } else {
        (1.0 - value).ln()
    }
}

/// Per-state chance the interval ends here, from a Gaussian interval density
/// about the period. An estimate with no period allows a beat anywhere, at a
/// uniform floor: a stretch with no periodicity must stay traversable.
fn hazard<S>(period: f32, states: usize, pools: &PoolRegion<S>) -> Result<SampleBuffer, PoolError>
where
    S: HasPool<f32>,
{
    if period <= 0.0 {
        let mut flat = pools.get_with_len::<f32>(states)?;
        flat.fill(Consts::EPSILON);
        return Ok(flat);
    }
    let sigma = frames::sigma();
    let support = (Consts::SUPPORT * sigma).ceil();
    let peak = Consts::DENSITY_SCALE / (frames::SIGMA_SECONDS * std::f32::consts::TAU.sqrt());
    let interval = collected(
        pools,
        states,
        (0..states).map(|state| {
            let gap = (state + 1).to_f32().unwrap_or(0.0) - period;
            if gap.abs() > support {
                0.0
            } else {
                peak * (-gap * gap / (2.0 * sigma * sigma)).exp()
            }
        }),
    )?;
    let mut remaining = 1.0f32;
    collected(
        pools,
        states,
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
