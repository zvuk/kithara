use crate::{BeatArtifact, artifact::MarkedBeat};

struct Consts;

impl Consts {
    const MAX_BPM: f64 = 180.0;
    /// Dub and ambient are played and counted in the low sixties.
    const MIN_BPM: f64 = 60.0;
    const STEPS: i32 = 3;
}

/// Names the same music at the level it is counted in. A tracker settles on
/// whichever level its evidence favours, so a steady track can come back at
/// half or twice the rate it is played at; the marks move with the number,
/// because a grid drawn at one level under a number at another is two answers.
pub(crate) fn fold(grid: BeatArtifact) -> BeatArtifact {
    let Some(steps) = steps(grid.bpm()) else {
        return grid;
    };
    let (beats, downbeats) = shift(
        marked(grid.beats(), grid.beat_confidence()),
        marked(grid.downbeats(), grid.downbeat_confidence()),
        steps,
    );
    BeatArtifact::with_regions(
        grid.bpm() * f64::from(2i32).powi(steps),
        beats,
        downbeats,
        grid.regions().to_vec(),
    )
}

fn steps(bpm: f64) -> Option<i32> {
    if !bpm.is_finite() || bpm <= 0.0 {
        return None;
    }
    let mut tempo = bpm;
    let mut steps = 0;
    while tempo < Consts::MIN_BPM && steps < Consts::STEPS {
        tempo *= 2.0;
        steps += 1;
    }
    while tempo >= Consts::MAX_BPM && steps > -Consts::STEPS {
        tempo /= 2.0;
        steps -= 1;
    }
    (steps != 0).then_some(steps)
}

fn marked(frames: &[u64], confidence: &[Option<f32>]) -> Vec<MarkedBeat> {
    frames
        .iter()
        .copied()
        .zip(confidence.iter().copied().chain(std::iter::repeat(None)))
        .collect()
}

fn shift(
    mut beats: Vec<MarkedBeat>,
    mut downbeats: Vec<MarkedBeat>,
    steps: i32,
) -> (Vec<MarkedBeat>, Vec<MarkedBeat>) {
    for _ in 0..steps.abs() {
        if steps > 0 {
            beats = split(&beats);
            downbeats = split(&downbeats);
        } else {
            beats = thin(&beats, phase(&beats, &downbeats));
            downbeats = thin(&downbeats, 0);
        }
    }
    (beats, downbeats)
}

/// One mark between every pair. An inserted mark was never detected, so it
/// carries no confidence, the way an extrapolated one does not.
fn split(marks: &[MarkedBeat]) -> Vec<MarkedBeat> {
    let mut out: Vec<MarkedBeat> = Vec::with_capacity(marks.len().saturating_mul(2));
    for pair in marks.windows(2) {
        out.push(pair[0]);
        out.push((pair[0].0 + (pair[1].0 - pair[0].0) / 2, None));
    }
    out.extend(marks.last().copied());
    out
}

fn thin(marks: &[MarkedBeat], phase: usize) -> Vec<MarkedBeat> {
    marks.iter().skip(phase).step_by(2).copied().collect()
}

/// Which of the two beats to keep, so that bar starts stay on a beat.
fn phase(beats: &[MarkedBeat], downbeats: &[MarkedBeat]) -> usize {
    downbeats
        .first()
        .and_then(|(bar, _)| beats.iter().position(|(beat, _)| beat == bar))
        .unwrap_or(0)
        % 2
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    const RATE: u64 = 44_100;

    fn grid(bpm: f64, beats_per_bar: usize, count: usize) -> BeatArtifact {
        let period = (60.0 / bpm * RATE as f64) as u64;
        let beats: Vec<MarkedBeat> = (0..count).map(|n| (n as u64 * period, Some(0.5))).collect();
        let downbeats: Vec<MarkedBeat> = beats.iter().step_by(beats_per_bar).copied().collect();
        BeatArtifact::new(bpm, beats, downbeats)
    }

    fn gaps(frames: &[u64]) -> Vec<u64> {
        frames.windows(2).map(|pair| pair[1] - pair[0]).collect()
    }

    #[kithara::test]
    fn a_half_time_grid_is_counted_at_the_rate_it_is_played() {
        let folded = fold(grid(55.0, 4, 17));

        assert!((folded.bpm() - 110.0).abs() < 1e-9);
        assert_eq!(folded.beats().len(), 33, "one mark between every pair");
        let gaps = gaps(folded.beats());
        assert!(
            gaps.windows(2).all(|pair| pair[0].abs_diff(pair[1]) <= 1),
            "the doubled grid stays even: {gaps:?}"
        );
        assert_eq!(
            folded.downbeats().len(),
            9,
            "bars double with the beats, so a bar keeps its four"
        );
    }

    #[kithara::test]
    fn a_double_time_grid_is_counted_at_the_rate_it_is_played() {
        let folded = fold(grid(200.0, 4, 17));

        assert!((folded.bpm() - 100.0).abs() < 1e-9);
        assert_eq!(folded.beats().len(), 9, "every other mark");
        assert!(
            folded
                .downbeats()
                .iter()
                .all(|bar| folded.beats().contains(bar)),
            "a bar start that is no longer a beat is not a bar start"
        );
    }

    #[kithara::test]
    fn a_grid_already_counted_that_way_is_left_alone() {
        let before = grid(128.0, 4, 17);
        let after = fold(grid(128.0, 4, 17));

        assert_eq!(before, after);
    }

    #[kithara::test]
    fn an_inserted_mark_claims_nothing() {
        let folded = fold(grid(55.0, 4, 5));

        assert!(
            folded
                .beat_confidence()
                .iter()
                .skip(1)
                .step_by(2)
                .all(Option::is_none),
            "a mark nothing detected carries no confidence"
        );
    }

    #[kithara::test]
    fn a_tempo_no_tracker_reports_is_left_alone() {
        let silent = BeatArtifact::new(0.0, Vec::new(), Vec::new());

        assert_eq!(fold(silent), BeatArtifact::new(0.0, Vec::new(), Vec::new()));
    }
}
