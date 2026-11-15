use std::{f64::consts::TAU, ops::Range};

use kithara_beat::GridBeat;
use num_traits::ToPrimitive;

use super::ORDINAL_TOLERANCE_BEATS;
use crate::consts;

type Stretch<'a> = (&'a [(u64, GridBeat)], TempoLine);

#[derive(Clone, Copy)]
struct TempoLine {
    intercept: f64,
    period: f64,
}

impl TempoLine {
    fn coordinate(self, at: f64) -> f64 {
        (at - self.intercept) / self.period
    }

    fn phase(markers: &[(u64, GridBeat)], period: f64) -> Self {
        let origin = markers[0].1.at;
        let (sin, cos) = markers.iter().fold((0.0, 0.0), |(sin, cos), (_, beat)| {
            let angle = TAU * (beat.at - origin) / period;
            let (s, c) = angle.sin_cos();
            (sin + s, cos + c)
        });
        Self {
            intercept: origin + sin.atan2(cos) * period / TAU,
            period,
        }
    }

    fn refine(self, markers: &[(u64, GridBeat)]) -> Self {
        let (count, x, y) = markers
            .iter()
            .filter(|(_, beat)| self.supports(beat.at))
            .fold((0.0, 0.0, 0.0), |(n, x, y), (_, beat)| {
                (n + 1.0, x + self.coordinate(beat.at).round(), y + beat.at)
            });
        if count < 2.0 {
            return self;
        }
        let mean_x = x / count;
        let mean_y = y / count;
        let (variance, covariance) = markers
            .iter()
            .filter(|(_, beat)| self.supports(beat.at))
            .fold((0.0, 0.0), |(variance, covariance), (_, beat)| {
                let dx = self.coordinate(beat.at).round() - mean_x;
                (variance + dx * dx, covariance + dx * (beat.at - mean_y))
            });
        if variance <= 0.0 {
            return self;
        }
        let period = covariance / variance;
        if !period.is_finite() || period <= 0.0 {
            return self;
        }
        Self {
            intercept: mean_y - period * mean_x,
            period,
        }
    }

    fn residual(self, at: f64) -> f64 {
        let beat = self.coordinate(at);
        (beat - beat.round()).abs() * self.period
    }

    fn score(self, markers: &[(u64, GridBeat)]) -> (usize, f64) {
        markers.iter().fold((0, 0.0), |(count, error), (_, beat)| {
            let residual = self.residual(beat.at);
            if residual <= consts::STEADY_TOLERANCE_SECONDS {
                (count + 1, error + residual * residual)
            } else {
                (count, error)
            }
        })
    }

    fn snap(self, mut beat: GridBeat) -> Option<GridBeat> {
        let coordinate = self.coordinate(beat.at);
        if (coordinate - coordinate.round()).abs() > ORDINAL_TOLERANCE_BEATS {
            return None;
        }
        beat.ordinal = coordinate.round().to_i64()?;
        beat.at = ((self.intercept + coordinate.round() * self.period) * consts::NANOS_PER_SECOND)
            .round()
            / consts::NANOS_PER_SECOND;
        (beat.at >= 0.0).then_some(beat)
    }

    fn supports(self, at: f64) -> bool {
        self.residual(at) <= consts::STEADY_TOLERANCE_SECONDS
    }
}

fn search(markers: &[(u64, GridBeat)], seed: f64) -> TempoLine {
    const PERIOD_SEARCH_RATIO: f64 = 0.04;
    const SEARCH_DRIFT_FRACTION: f64 = 0.25;

    let mut best = TempoLine::phase(markers, seed);
    let mut score = best.score(markers);
    let span = markers[markers.len() - 1].1.at - markers[0].1.at;
    if span > 0.0 {
        let step = consts::STEADY_TOLERANCE_SECONDS * seed / span * SEARCH_DRIFT_FRACTION;
        let mut period = seed * (1.0 - PERIOD_SEARCH_RATIO);
        while period <= seed * (1.0 + PERIOD_SEARCH_RATIO) {
            let line = TempoLine::phase(markers, period);
            let candidate = line.score(markers);
            if candidate.0 > score.0 || (candidate.0 == score.0 && candidate.1 < score.1) {
                best = line;
                score = candidate;
            }
            let next = period + step;
            if next <= period {
                break;
            }
            period = next;
        }
    }
    best.refine(markers)
}

fn departure(markers: &[(u64, GridBeat)], line: TempoLine) -> Option<Range<usize>> {
    let window = consts::MIN_STEADY_BEATS;
    let mut run: Option<Range<usize>> = None;
    let mut longest: Option<Range<usize>> = None;
    let mut on_line = 0;
    for (index, (_, beat)) in markers.iter().enumerate() {
        on_line += usize::from(line.supports(beat.at));
        if index >= window {
            on_line -= usize::from(line.supports(markers[index - window].1.at));
        }
        if index + 1 >= window && on_line * 2 < window {
            let start = index + 1 - window;
            let range = run.get_or_insert(start..index + 1);
            range.end = index + 1;
            if longest.as_ref().is_none_or(|best| range.len() > best.len()) {
                longest = Some(range.clone());
            }
        } else {
            run = None;
        }
    }
    longest
}

fn separate(markers: &[(u64, GridBeat)], parent: TempoLine, child: TempoLine) -> Range<usize> {
    let mut start = 0;
    let mut gain = 0.0;
    let mut best_gain = 0.0;
    let mut best = 0..markers.len();
    for (index, (_, beat)) in markers.iter().enumerate() {
        let old = parent
            .residual(beat.at)
            .min(consts::STEADY_TOLERANCE_SECONDS);
        let new = child
            .residual(beat.at)
            .min(consts::STEADY_TOLERANCE_SECONDS);
        gain += old * old - new * new;
        if gain > best_gain {
            best_gain = gain;
            best = start..index + 1;
        }
        if gain < 0.0 {
            start = index + 1;
            gain = 0.0;
        }
    }
    best
}

fn stretches<'a>(markers: &'a [(u64, GridBeat)], seed: f64, out: &mut Vec<Stretch<'a>>) {
    if markers.is_empty() {
        return;
    }
    let line = search(markers, seed);
    if let Some(range) = departure(markers, line)
        && range.len() < markers.len()
    {
        let changed = &markers[range.clone()];
        let departures: Vec<_> = changed
            .iter()
            .copied()
            .filter(|(_, beat)| !line.supports(beat.at))
            .collect();
        if departures.len() >= 2 {
            let mut gaps: Vec<_> = departures
                .windows(2)
                .map(|pair| {
                    let gap = pair[1].1.at - pair[0].1.at;
                    gap / (gap / line.period).round().max(1.0)
                })
                .collect();
            gaps.sort_by(f64::total_cmp);
            let other = search(&departures, gaps[gaps.len() / 2]);
            let support = other.score(changed).0;
            let bounds = separate(changed, line, other);
            let seam = other.intercept
                + other.coordinate(changed[bounds.start].1.at).round() * other.period;
            if support >= consts::MIN_STEADY_BEATS
                && support > line.score(changed).0
                && line.supports(seam)
            {
                let range = range.start + bounds.start..range.start + bounds.end;
                stretches(&markers[..range.start], line.period, out);
                out.push((
                    &markers[range.clone()],
                    other.refine(&markers[range.clone()]),
                ));
                stretches(&markers[range.end..], line.period, out);
                return;
            }
        }
    }
    out.push((markers, line));
}

pub(super) fn place(markers: &[(u64, GridBeat)], bpm: f64) -> (Vec<(u64, GridBeat)>, f64) {
    let seed = consts::SECONDS_PER_MINUTE / bpm;
    let mut pieces = Vec::new();
    stretches(markers, seed, &mut pieces);
    let mut kept: Vec<(u64, GridBeat)> = Vec::with_capacity(markers.len());
    let mut longest = 0;
    let mut tempo = bpm;
    for (markers, line) in pieces {
        let count = append(markers, line, &mut kept);
        if count > longest {
            longest = count;
            tempo = consts::SECONDS_PER_MINUTE / line.period;
        }
    }
    (kept, tempo)
}

fn append(markers: &[(u64, GridBeat)], line: TempoLine, kept: &mut Vec<(u64, GridBeat)>) -> usize {
    let mut snapped = markers
        .iter()
        .filter_map(|(frame, beat)| line.snap(*beat).map(|beat| (*frame, beat)));
    let Some(first) = snapped.next() else {
        return 0;
    };
    let shift = match kept.last() {
        Some((_, last)) => ((first.1.at - last.at) / line.period)
            .round()
            .max(1.0)
            .to_i64()
            .and_then(|step| last.ordinal.checked_add(step))
            .and_then(|ordinal| ordinal.checked_sub(first.1.ordinal)),
        None => first.1.ordinal.checked_neg(),
    };
    let Some(shift) = shift else {
        return 0;
    };
    let start = kept.len();
    for (frame, mut beat) in std::iter::once(first).chain(snapped) {
        let Some(ordinal) = beat.ordinal.checked_add(shift) else {
            continue;
        };
        beat.ordinal = ordinal;
        if kept
            .last()
            .is_some_and(|(_, last)| beat.ordinal <= last.ordinal || beat.at <= last.at)
        {
            continue;
        }
        kept.push((frame, beat));
    }
    kept.len() - start
}
