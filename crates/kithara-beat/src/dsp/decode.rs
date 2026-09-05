use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
use num_traits::cast::ToPrimitive;

use super::{
    buffer::collected,
    consts::{DecodeConsts, FramesConsts, PeriodConsts},
    frames,
};

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
    let states = (longest + DecodeConsts::STATE_MARGIN * frames::sigma())
        .floor()
        .to_usize()
        .unwrap_or(0);
    if states < 2 || curve.is_empty() {
        return Ok((pools.get::<f32>(), f32::NEG_INFINITY));
    }

    let observation = normalise(curve, pools)?;
    let hazards: Vec<(SampleBuffer, SampleBuffer)> = periods
        .iter()
        .map(|&p| log_hazard(p, states, pools))
        .collect::<Result<_, _>>()?;
    // The path starts as if a beat fell just before the first frame.
    let mut previous = pools.get_with_len::<f32>(states)?;
    previous.fill(f32::NEG_INFINITY);
    if let Some(first) = previous.first_mut() {
        *first = 0.0;
    }
    let mut current = pools.get_with_len::<f32>(states)?;
    current.fill(f32::NEG_INFINITY);
    let mut back = vec![0u16; curve.len()];

    for frame in 0..curve.len() {
        let (fire, stay) = &hazards[estimate_for(frame, hazards.len())];
        let (from, score) = previous
            .iter()
            .enumerate()
            .map(|(state, value)| (state, value + fire[state]))
            .fold((0usize, f32::NEG_INFINITY), |best, next| {
                if next.1 > best.1 { next } else { best }
            });
        current[0] = score + observation[frame].ln();
        back[frame] = u16::try_from(from).unwrap_or(0);
        let missed = (1.0 - observation[frame]).ln();
        for state in 1..states {
            current[state] = previous[state - 1] + stay[state - 1] + missed;
        }
        std::mem::swap(&mut previous, &mut current);
    }

    let optimum = previous.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    Ok((backtrack(&previous, &back, curve.len(), pools)?, optimum))
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
    let states = (longest + DecodeConsts::STATE_MARGIN * frames::sigma())
        .floor()
        .to_usize()
        .unwrap_or(0);
    let hazards = periods
        .iter()
        .map(|&p| hazard(p, states, pools))
        .collect::<Result<_, _>>()?;
    Ok((beats, optimum, hazards, normalise(curve, pools)?, states))
}

/// The estimate measured over the window opening at step `k` applies during
/// step `k + 1`.
pub(super) fn estimate_for(frame: usize, estimates: usize) -> usize {
    (frame / PeriodConsts::ACF_STEP)
        .saturating_sub(1)
        .min(estimates - 1)
}

fn backtrack<S>(
    last: &[f32],
    back: &[u16],
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
            state = back[frame].into();
        } else {
            state -= 1;
        }
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
        flat.fill(DecodeConsts::EPSILON);
        return Ok(flat);
    }
    collected(
        pools,
        curve.len(),
        curve.iter().map(|value| {
            (DecodeConsts::OBSERVED_CEILING * value / peak).max(DecodeConsts::EPSILON)
        }),
    )
}

/// A hazard in the log domain: per state, the chance the interval ends here
/// and the chance it runs on. An estimate holds for `ACF_STEP` frames, so the
/// decoder reads these and never takes a logarithm in its loop.
fn log_hazard<S>(
    period: f32,
    states: usize,
    pools: &PoolRegion<S>,
) -> Result<(SampleBuffer, SampleBuffer), PoolError>
where
    S: HasPool<f32>,
{
    let hazard = hazard(period, states, pools)?;
    let fire = collected(pools, states, hazard.iter().map(|chance| chance.ln()))?;
    let stay = collected(
        pools,
        states,
        hazard.iter().map(|chance| (1.0 - chance).ln()),
    )?;
    Ok((fire, stay))
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
        flat.fill(DecodeConsts::EPSILON);
        return Ok(flat);
    }
    let sigma = frames::sigma();
    let support = (DecodeConsts::SUPPORT * sigma).ceil();
    let peak =
        DecodeConsts::DENSITY_SCALE / (FramesConsts::SIGMA_SECONDS * std::f32::consts::TAU.sqrt());
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
