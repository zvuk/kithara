use num_traits::cast::AsPrimitive;

use crate::api::BeatError;

/// Frames per second of the beat model output.
const FPS: f32 = 50.0;

/// Decodes raw beat/downbeat logits into timestamped events: max-pool peak
/// picking, thresholding, deduplication, downbeat-to-beat snapping.
pub(crate) struct PeakPicker {
    fps: f32,
}

impl Default for PeakPicker {
    fn default() -> Self {
        Self { fps: FPS }
    }
}

impl PeakPicker {
    /// Decode beat and downbeat logits into `(beats, downbeats)` in seconds.
    ///
    /// Both slices must have the same length (one value per mel frame).
    pub(crate) fn decode(
        &self,
        beat_logits: &[f32],
        downbeat_logits: &[f32],
    ) -> Result<(Vec<f32>, Vec<f32>), BeatError> {
        if beat_logits.len() != downbeat_logits.len() {
            return Err(BeatError::Inference {
                reason: format!(
                    "beat_logits length ({}) != downbeat_logits length ({})",
                    beat_logits.len(),
                    downbeat_logits.len()
                ),
            });
        }

        let beat_frames = find_peaks(beat_logits);
        let downbeat_frames = find_peaks(downbeat_logits);

        let beats: Vec<f32> = beat_frames
            .iter()
            .map(|&f| (f / f64::from(self.fps)).as_())
            .collect();
        let mut downbeats: Vec<f32> = downbeat_frames
            .iter()
            .map(|&f| (f / f64::from(self.fps)).as_())
            .collect();

        snap_downbeats_to_beats(&beats, &mut downbeats);

        Ok((beats, downbeats))
    }
}

/// Identify local maxima exceeding the logit threshold (> 0.0).
///
/// Max-pool window of 7 frames (±3), stride 1: a frame is a peak if it equals
/// the local maximum and is positive.
fn find_peaks(logits: &[f32]) -> Vec<f64> {
    let len = logits.len();
    let mut peaks = Vec::new();

    for i in 0..len {
        // logit > 0 corresponds to probability > 0.5 after sigmoid.
        if logits[i] <= 0.0 {
            continue;
        }

        let start = i.saturating_sub(3);
        let end = (i + 4).min(len);

        let mut is_max = true;
        for j in start..end {
            if logits[j] > logits[i] {
                is_max = false;
                break;
            }
        }

        if is_max {
            peaks.push(i);
        }
    }

    deduplicate_peaks(&peaks, 1)
}

/// Merge adjacent peak frame indices using a running mean.
///
/// Consecutive peaks at most `width` frames apart collapse to their mean
/// position.
fn deduplicate_peaks(peaks: &[usize], width: usize) -> Vec<f64> {
    let Some((&first, rest)) = peaks.split_first() else {
        return Vec::new();
    };

    let mut result = Vec::new();
    let mut p: f64 = first.as_();
    let mut c = 1.0_f64;

    for &p2_usize in rest {
        let p2: f64 = p2_usize.as_();
        if p2 - p <= width.as_() {
            c += 1.0;
            p += (p2 - p) / c;
        } else {
            result.push(p);
            p = p2;
            c = 1.0;
        }
    }
    result.push(p);

    result
}

fn snap_downbeats_to_beats(beat_times: &[f32], downbeat_times: &mut Vec<f32>) {
    if beat_times.is_empty() || downbeat_times.is_empty() {
        return;
    }

    for d_time in downbeat_times.iter_mut() {
        let pos = beat_times.partition_point(|&b| b < *d_time);

        let best = match (pos.checked_sub(1), beat_times.get(pos)) {
            (Some(before), Some(&after)) => {
                if (*d_time - beat_times[before]).abs() <= (after - *d_time).abs() {
                    beat_times[before]
                } else {
                    after
                }
            }
            (Some(before), None) => beat_times[before],
            (None, Some(&after)) => after,
            (None, None) => continue,
        };

        *d_time = best;
    }

    downbeat_times.sort_by(f32::total_cmp);
    downbeat_times.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_peaks_single_peak() {
        let logits = [0.0, 0.0, 0.5, 1.0, 0.5, 0.0, 0.0];
        let peaks = find_peaks(&logits);
        assert_eq!(peaks, vec![3.0]);
    }

    #[test]
    fn find_peaks_below_threshold() {
        let logits = [-1.0, -0.5, -2.0, -0.1];
        let peaks = find_peaks(&logits);
        assert!(peaks.is_empty());
    }

    #[test]
    fn find_peaks_multiple_peaks() {
        // Two peaks separated by more than 3 frames.
        let mut logits = vec![0.0; 20];
        logits[3] = 2.0;
        logits[15] = 1.5;
        let peaks = find_peaks(&logits);
        assert_eq!(peaks, vec![3.0, 15.0]);
    }

    #[test]
    fn find_peaks_window_suppresses_smaller_neighbour() {
        // A smaller positive value 3 frames from a larger one is not a peak.
        let mut logits = vec![0.0; 10];
        logits[4] = 2.0;
        logits[7] = 1.0;
        let peaks = find_peaks(&logits);
        assert_eq!(peaks, vec![4.0]);
    }

    #[test]
    fn find_peaks_outside_window_both_survive() {
        // 4 frames apart: each is the max of its own ±3 window.
        let mut logits = vec![0.0; 10];
        logits[2] = 2.0;
        logits[6] = 1.0;
        let peaks = find_peaks(&logits);
        assert_eq!(peaks, vec![2.0, 6.0]);
    }

    #[test]
    fn find_peaks_plateau_collapses_to_centre() {
        // Adjacent frames with equal positive values: both tie the max-pool,
        // dedup merges them to the plateau centre.
        let logits = [0.0, 1.0, 1.0, 0.0];
        let peaks = find_peaks(&logits);
        assert_eq!(peaks.len(), 1);
        assert_eq!(peaks[0], 1.5);
    }

    #[test]
    fn deduplicate_peaks_empty() {
        let peaks = deduplicate_peaks(&[], 1);
        assert!(peaks.is_empty());
    }

    #[test]
    fn deduplicate_peaks_no_adjacent() {
        let peaks = deduplicate_peaks(&[5, 10, 20], 1);
        assert_eq!(peaks, vec![5.0, 10.0, 20.0]);
    }

    #[test]
    fn deduplicate_peaks_merge() {
        // 10 and 11 merge (gap 1) to 10.5; 12 is 1.5 from the mean → new group.
        let peaks = deduplicate_peaks(&[10, 11, 12, 20], 1);
        assert_eq!(peaks, vec![10.5, 12.0, 20.0]);

        // {10, 11, 11}: running mean 32/3, kept fractional.
        let peaks = deduplicate_peaks(&[10, 11, 11, 20], 1);
        assert_eq!(peaks.len(), 2);
        assert!((peaks[0] - 32.0 / 3.0).abs() < 1e-9);
        assert_eq!(peaks[1], 20.0);
    }

    #[test]
    fn deduplicate_peaks_single() {
        let peaks = deduplicate_peaks(&[42], 1);
        assert_eq!(peaks, vec![42.0]);
    }

    #[test]
    fn snap_downbeats() {
        let beats = vec![1.0, 2.0, 3.0];
        let mut downbeats = vec![1.1, 2.8];
        snap_downbeats_to_beats(&beats, &mut downbeats);
        assert_eq!(downbeats, vec![1.0, 3.0]);
    }

    #[test]
    fn snap_downbeats_dedup() {
        let beats = vec![1.0, 2.0, 3.0];
        // Both downbeats snap to 2.0 and collapse to one.
        let mut downbeats = vec![1.8, 2.1];
        snap_downbeats_to_beats(&beats, &mut downbeats);
        assert_eq!(downbeats, vec![2.0]);
    }

    #[test]
    fn snap_downbeats_empty_beats() {
        let beats: Vec<f32> = vec![];
        let mut downbeats = vec![1.0, 2.0];
        snap_downbeats_to_beats(&beats, &mut downbeats);
        assert_eq!(downbeats, vec![1.0, 2.0]);
    }

    #[test]
    fn snap_downbeats_empty_downbeats() {
        let beats = vec![1.0, 2.0];
        let mut downbeats: Vec<f32> = vec![];
        snap_downbeats_to_beats(&beats, &mut downbeats);
        assert!(downbeats.is_empty());
    }

    #[test]
    fn decode_full() {
        let mut beat_logits = vec![-5.0; 200];
        let mut downbeat_logits = vec![-5.0; 200];

        beat_logits[50] = 3.0;
        beat_logits[100] = 2.5;
        beat_logits[150] = 4.0;
        // Downbeat at frame 51 snaps to the beat at frame 50.
        downbeat_logits[51] = 2.0;

        let pp = PeakPicker::default();
        let (beats, downbeats) = pp.decode(&beat_logits, &downbeat_logits).unwrap();

        assert_eq!(beats, vec![1.0, 2.0, 3.0]);
        assert_eq!(downbeats, vec![1.0]);
    }

    #[test]
    fn decode_empty_logits() {
        let pp = PeakPicker::default();
        let (beats, downbeats) = pp.decode(&[], &[]).unwrap();
        assert!(beats.is_empty());
        assert!(downbeats.is_empty());
    }

    #[test]
    fn decode_mismatched_lengths() {
        let pp = PeakPicker::default();
        let err = pp.decode(&[1.0, 2.0], &[1.0]);
        assert!(err.is_err());
    }
}
