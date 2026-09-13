use num_traits::cast;

use super::{SAW_PERIOD, phase};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameClass {
    Ascending,
    Descending,
    Silence,
    Unknown,
}

/// Per-window classification of a mono f32 stream.
///
/// `window` = frames per window; `tol` = allowed deviation of the mean
/// per-frame modular delta from +/-1.0 i16 units.
///
/// A window's mean covers the step into its first frame as well as the steps
/// between its own frames, so the windows together read every step the stream
/// contains. Reading only the interior steps would drop one step per window --
/// precisely the steps that land on a window boundary -- and a splice that
/// landed there would be invisible to every caller.
#[must_use]
pub fn classify_windows(left: &[f32], window: usize, tol: f32) -> Vec<FrameClass> {
    if window < 2 {
        return Vec::new();
    }

    left.chunks_exact(window)
        .enumerate()
        .map(|(index, samples)| {
            let preceding = (index > 0).then(|| left[index * window - 1]);
            classify_window(samples, preceding, tol)
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
pub struct Replay {
    pub len: usize,
    pub start_frame: usize,
    pub start_phase: usize,
}

/// Detect phase-continuity violations inside a contiguous ascending sawtooth.
#[must_use]
pub fn ascending_phase_replays(
    left: &[f32],
    start: usize,
    end: usize,
    tol_units: i32,
) -> Vec<Replay> {
    let end = end.min(left.len());
    if start >= end {
        return Vec::new();
    }

    let base_phase = phase::units(left[start]);
    let mut replays = Vec::new();
    let mut active: Option<Replay> = None;

    for (offset, sample) in left[start..end].iter().copied().enumerate() {
        let frame = start + offset;
        let is_violation = !is_silence(sample)
            && expected_phase_error(sample, base_phase, offset).abs() > tol_units;

        match (is_violation, active.as_mut()) {
            (true, Some(replay)) => {
                replay.len += 1;
            }
            (true, None) => {
                active = Some(Replay {
                    start_frame: frame,
                    len: 1,
                    start_phase: phase::units(sample),
                });
            }
            (false, Some(_)) => {
                if let Some(replay) = active.take() {
                    replays.push(replay);
                }
            }
            (false, None) => {}
        }
    }

    if let Some(replay) = active {
        replays.push(replay);
    }

    replays
}

fn classify_window(samples: &[f32], preceding: Option<f32>, tol: f32) -> FrameClass {
    if samples.iter().all(|sample| is_silence(*sample)) {
        return FrameClass::Silence;
    }

    let entry = preceding.map(|prior| step(prior, samples[0]));
    let delta_sum = entry
        .into_iter()
        .chain(samples.windows(2).map(|pair| step(pair[0], pair[1])))
        .sum::<f32>();
    let count = samples.len() - 1 + usize::from(entry.is_some());
    let steps: f32 = cast(count).expect("invariant: a window length fits f32");
    let mean_delta = delta_sum / steps;

    if (mean_delta - 1.0).abs() <= tol {
        FrameClass::Ascending
    } else if (mean_delta + 1.0).abs() <= tol {
        FrameClass::Descending
    } else {
        FrameClass::Unknown
    }
}

fn step(from: f32, to: f32) -> f32 {
    f32::from(phase::delta(phase::units(from), phase::units(to)))
}

fn expected_phase_error(sample: f32, base_phase: usize, frame_offset: usize) -> i32 {
    let expected = (base_phase + frame_offset % SAW_PERIOD) % SAW_PERIOD;
    i32::from(phase::delta(expected, phase::units(sample)))
}

fn is_silence(sample: f32) -> bool {
    const SILENCE_THRESHOLD: f32 = 1.0e-4;
    sample.abs() < SILENCE_THRESHOLD
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{FrameClass, Replay, SAW_PERIOD, ascending_phase_replays, classify_windows};
    use crate::fixtures::{
        ascending_pcm, ascending_wrap_pcm, descending_pcm, descending_wrap_pcm, provenance_silence,
    };

    const WINDOW: usize = 64;

    #[kithara::test(native, flash(false))]
    fn classify_windows_labels_pure_signals_including_wrap(
        ascending_wrap_pcm: Vec<f32>,
        descending_wrap_pcm: Vec<f32>,
        provenance_silence: Vec<f32>,
    ) {
        let ascending = &ascending_wrap_pcm[..WINDOW];
        assert_eq!(
            classify_windows(ascending, WINDOW, 0.5),
            vec![FrameClass::Ascending]
        );

        let descending = descending_wrap_pcm;
        assert_eq!(
            classify_windows(&descending, WINDOW, 0.5),
            vec![FrameClass::Descending]
        );

        let silence = provenance_silence;
        assert_eq!(
            classify_windows(&silence, WINDOW, 0.5),
            vec![FrameClass::Silence]
        );
    }

    /// A splice is a splice wherever it falls. The frame it lands on is set by
    /// whatever the renderer had committed when the flush arrived, so a reader
    /// that saw only the steps inside a window would report the same stream as
    /// continuous in one run and broken in the next.
    #[kithara::test(native, flash(false))]
    fn a_splice_on_a_window_boundary_still_breaks_the_class(ascending_pcm: Vec<f32>) {
        const JUMP: usize = SAW_PERIOD / 2;

        let mut left = ascending_pcm[..WINDOW * 2].to_vec();
        left[WINDOW..].copy_from_slice(&ascending_pcm[JUMP..JUMP + WINDOW]);

        assert_eq!(
            classify_windows(&left, WINDOW, 0.5),
            vec![FrameClass::Ascending, FrameClass::Unknown]
        );
    }

    #[kithara::test(native, flash(false))]
    fn ascending_phase_replays_accepts_pure_ascending_run_including_wrap(
        ascending_wrap_pcm: Vec<f32>,
    ) {
        let left = ascending_wrap_pcm;

        assert!(ascending_phase_replays(&left, 0, left.len(), 3).is_empty());
    }

    #[kithara::test(native, flash(false))]
    fn ascending_phase_replays_reports_spliced_replay_from_start(ascending_pcm: Vec<f32>) {
        let splice_start = 200_000;
        let replay_len = 1_000;
        let total_len = splice_start + replay_len + 2_000;
        let mut left = ascending_pcm[..total_len].to_vec();
        let replay = &ascending_pcm[..replay_len];
        left[splice_start..splice_start + replay_len].copy_from_slice(replay);

        let replays = ascending_phase_replays(&left, 0, left.len(), 3);

        assert_eq!(replays.len(), 1);
        assert_replay(
            replays[0],
            Replay {
                start_frame: splice_start,
                len: replay_len,
                start_phase: 0,
            },
        );
    }

    #[kithara::test(native, flash(false))]
    fn ascending_phase_replays_reports_descending_region(
        ascending_pcm: Vec<f32>,
        descending_pcm: Vec<f32>,
    ) {
        let descending_start = 128;
        let descending_len = 64;
        let mut left = ascending_pcm[..512].to_vec();
        let descending = descending_pcm;
        left[descending_start..descending_start + descending_len].copy_from_slice(&descending);

        let replays = ascending_phase_replays(&left, 0, left.len(), 3);

        assert_eq!(replays.len(), 1);
        assert_replay(
            replays[0],
            Replay {
                start_frame: descending_start,
                len: descending_len,
                start_phase: 65_535,
            },
        );
    }

    fn assert_replay(actual: Replay, expected: Replay) {
        assert_eq!(actual.start_frame, expected.start_frame);
        assert_eq!(actual.len, expected.len);
        assert_eq!(actual.start_phase, expected.start_phase);
    }
}
