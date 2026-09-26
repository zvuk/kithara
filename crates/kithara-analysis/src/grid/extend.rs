use num_traits::cast::ToPrimitive;

use crate::{
    BeatArtifact,
    artifact::{MarkedBeat, voted_bar},
};

pub(crate) fn extend_over(grid: BeatArtifact, extent: u64, source_rate: u32) -> BeatArtifact {
    let Some(beat) = beat_period(grid.bpm(), source_rate) else {
        return grid;
    };
    let beats = spread(grid.beats(), grid.beat_confidence(), beat, extent);
    let downbeats = bar_lines(&beats, grid.downbeats(), grid.downbeat_confidence());

    BeatArtifact::with_regions(grid.bpm(), beats, downbeats, grid.regions().to_vec())
}

fn beat_period(bpm: f64, source_rate: u32) -> Option<f64> {
    if bpm <= 0.0 {
        return None;
    }
    Some(60.0 / bpm * f64::from(source_rate))
}

/// Every spread beat on the bar the detected bar lines agree on, keeping what
/// the detector said about the ones it heard.
///
/// A bar line is a beat, so it is counted in beats of the spread grid rather
/// than spread at a period of its own. Bar lines that agree on no bar, or sit
/// on no beat because the detector named too few, are left as heard: nothing
/// proves where the ones between them fall.
fn bar_lines(beats: &[MarkedBeat], marks: &[u64], confidence: &[Option<f32>]) -> Vec<MarkedBeat> {
    let heard: Vec<(usize, Option<f32>)> = marks
        .iter()
        .zip(confidence)
        .filter_map(|(frame, confidence)| {
            let index = beats.binary_search_by_key(frame, |(beat, _)| *beat).ok()?;
            Some((index, *confidence))
        })
        .collect();
    let votes = heard
        .iter()
        .filter(|(_, confidence)| confidence.is_some())
        .filter_map(|(index, _)| index.to_i64());
    let Some((bar, phase)) = voted_bar(votes) else {
        return marks
            .iter()
            .copied()
            .zip(confidence.iter().copied())
            .collect();
    };
    beats
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            index
                .to_i64()
                .is_some_and(|index| index.rem_euclid(bar) == phase)
        })
        .map(|(index, (frame, _))| {
            let confidence = heard
                .binary_search_by_key(&index, |(heard, _)| *heard)
                .ok()
                .and_then(|found| heard[found].1);
            (*frame, confidence)
        })
        .collect()
}

fn spread(marks: &[u64], confidence: &[Option<f32>], period: f64, extent: u64) -> Vec<MarkedBeat> {
    if period <= 0.0 {
        return marks
            .iter()
            .enumerate()
            .map(|(index, frame)| (*frame, confidence.get(index).copied().flatten()))
            .collect();
    }
    let Some((first, last)) = marks.first().zip(marks.last()) else {
        return Vec::<MarkedBeat>::new();
    };

    let mut out: Vec<MarkedBeat> = Vec::with_capacity(marks.len());
    let Some(anchor) = first.to_f64() else {
        return out;
    };
    let mut step = 1.0;
    while anchor - step * period >= 0.0 {
        if let Some(at) = (anchor - step * period).to_u64() {
            out.push((at, None));
        }
        step += 1.0;
    }
    out.reverse();

    for (index, pair) in marks.windows(2).enumerate() {
        out.push((pair[0], confidence.get(index).copied().flatten()));
        fill_between(&mut out, pair[0], pair[1], period);
    }
    out.push((
        *last,
        confidence
            .get(marks.len().saturating_sub(1))
            .copied()
            .flatten(),
    ));

    let mut step = 1.0;
    while let Some(at) = offset(*last, step * period) {
        if at >= extent {
            break;
        }
        out.push((at, None));
        step += 1.0;
    }
    out
}

fn fill_between(out: &mut Vec<MarkedBeat>, from: u64, to: u64, period: f64) {
    const FILL_THRESHOLD: f64 = 1.5;

    let Some(gap) = to.checked_sub(from).and_then(|gap| gap.to_f64()) else {
        return;
    };
    let steps = (gap / period).round();
    if steps < FILL_THRESHOLD {
        return;
    }
    let Some(anchor) = from.to_f64() else {
        return;
    };
    let stride = gap / steps;
    let mut step = 1.0;
    while step < steps {
        if let Some(at) = (anchor + step * stride).to_u64() {
            out.push((at, None));
        }
        step += 1.0;
    }
}

fn offset(from: u64, by: f64) -> Option<u64> {
    from.to_f64().and_then(|at| (at + by).to_u64())
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::extend_over;
    use crate::{BeatArtifact, consts};

    fn grid(beats: Vec<u64>) -> BeatArtifact {
        let downbeats = beats.iter().step_by(4).map(detected).collect();
        BeatArtifact::new(120.0, beats.iter().map(detected).collect(), downbeats)
    }

    fn detected(frame: &u64) -> (u64, Option<f32>) {
        (*frame, Some(0.9))
    }

    #[kithara::test]
    fn an_extrapolated_marker_claims_nothing_and_a_detected_one_keeps_its_answer() {
        let detected = vec![0, 22_050, 44_100, 66_150];
        let out = extend_over(
            grid(detected.clone()),
            10 * u64::from(consts::EXTEND_RATE),
            consts::EXTEND_RATE,
        );

        assert!(
            out.beats().len() > detected.len(),
            "the grid reached past what was detected"
        );
        for (&frame, &confidence) in out.beats().iter().zip(out.beat_confidence()) {
            if detected.contains(&frame) {
                assert_eq!(
                    confidence,
                    Some(0.9),
                    "a detected marker keeps what the detector said about it"
                );
            } else {
                assert_eq!(
                    confidence, None,
                    "a marker at {frame} nothing detected claims no confidence"
                );
            }
        }
    }

    #[kithara::test]
    fn a_short_run_of_markers_covers_the_whole_extent() {
        // Four beats near the start of a ten-second track.
        let detected = vec![0, 22_050, 44_100, 66_150];
        let out = extend_over(
            grid(detected.clone()),
            10 * u64::from(consts::EXTEND_RATE),
            consts::EXTEND_RATE,
        );

        for beat in &detected {
            assert!(
                out.beats().contains(beat),
                "a detected marker must survive: {beat}"
            );
        }
        assert!(
            out.beats().len() >= 19,
            "ten seconds at 120 bpm is about twenty beats, got {}",
            out.beats().len()
        );
        assert!(
            out.beats().windows(2).all(|pair| pair[1] > pair[0]),
            "markers must stay ascending"
        );
        assert!(
            out.beats().last().is_some_and(|last| *last < 441_000),
            "extrapolation must stop at the extent"
        );
    }

    #[kithara::test]
    fn markers_before_the_first_detection_are_filled_in() {
        // The first covered piece starts two seconds in.
        let out = extend_over(
            grid(vec![88_200, 110_250]),
            5 * u64::from(consts::EXTEND_RATE),
            consts::EXTEND_RATE,
        );
        assert!(
            out.beats().first().is_some_and(|first| *first < 88_200),
            "the run before the first detection must be filled: {:?}",
            out.beats().first()
        );
    }

    #[kithara::test]
    fn a_gap_between_detections_is_divided_evenly() {
        // Two detected pieces four beats apart.
        let out = extend_over(
            grid(vec![0, 22_050, 110_250, 132_300]),
            132_300,
            consts::EXTEND_RATE,
        );
        assert_eq!(
            out.beats(),
            &[0, 22_050, 44_100, 66_150, 88_200, 110_250, 132_300],
            "the gap must be divided at the observed period"
        );
    }

    /// Bar lines are beats: the spread ones land on spread beats at the bar
    /// the detected ones agree on, not at whatever spacing the first two
    /// happened to leave.
    #[kithara::test]
    fn spread_bar_lines_sit_on_beats_at_the_agreed_bar() {
        let beats: Vec<u64> = (0..24).map(|beat| beat * consts::EXTEND_BEAT).collect();
        let heard = [0, 8, 12, 16, 20];
        let out = extend_over(
            BeatArtifact::new(
                120.0,
                beats.iter().map(detected).collect(),
                heard
                    .iter()
                    .map(|beat| detected(&(beat * consts::EXTEND_BEAT)))
                    .collect(),
            ),
            24 * consts::EXTEND_BEAT,
            consts::EXTEND_RATE,
        );

        assert_eq!(
            out.downbeats(),
            (0..6)
                .map(|bar| bar * 4 * consts::EXTEND_BEAT)
                .collect::<Vec<_>>(),
            "every fourth beat is a bar line, the skipped one included"
        );
        assert!(
            out.downbeats().iter().all(|bar| out.beats().contains(bar)),
            "a bar line is always a beat"
        );
        assert_eq!(
            out.downbeat_confidence(),
            [Some(0.9), None, Some(0.9), Some(0.9), Some(0.9), Some(0.9)],
            "the bar nothing detected claims no confidence"
        );
    }

    #[kithara::test]
    fn a_grid_with_nothing_to_go_on_is_left_alone() {
        let empty = BeatArtifact::new(120.0, Vec::new(), Vec::new());
        assert!(
            extend_over(empty, 441_000, consts::EXTEND_RATE)
                .beats()
                .is_empty()
        );

        let zero_tempo = BeatArtifact::new(0.0, vec![(100, Some(0.9))], Vec::new());
        assert_eq!(
            extend_over(zero_tempo, 441_000, consts::EXTEND_RATE).beats(),
            &[100],
            "a single marker without a tempo cannot be spread"
        );
    }
}
