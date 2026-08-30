use num_traits::cast::AsPrimitive;

use crate::{
    mark::BeatMark,
    nn::{api::BeatError, config::BeatConfig},
};

pub(crate) struct PeakPicker {
    config: BeatConfig,
}

impl PeakPicker {
    pub(crate) fn new(config: BeatConfig) -> Self {
        Self { config }
    }

    pub(crate) fn decode(
        &self,
        beat_logits: &[f32],
        downbeat_logits: &[f32],
    ) -> Result<(Vec<BeatMark>, Vec<BeatMark>), BeatError> {
        if beat_logits.len() != downbeat_logits.len() {
            return Err(BeatError::Inference {
                reason: format!(
                    "beat_logits length ({}) != downbeat_logits length ({})",
                    beat_logits.len(),
                    downbeat_logits.len()
                ),
            });
        }

        let beats = find_marks(beat_logits, &self.config);
        let mut downbeats = find_marks(downbeat_logits, &self.config);

        snap_downbeats_to_beats(&beats, &mut downbeats);

        Ok((beats, downbeats))
    }
}

#[derive(Clone, Copy, Debug)]
struct Peak {
    logit: f32,
    at: f64,
}

impl Peak {
    fn mark(self) -> BeatMark {
        const FPS: f32 = 50.0;

        BeatMark {
            at: (self.at / f64::from(FPS)).as_(),
            confidence: sigmoid(self.logit),
        }
    }
}

fn sigmoid(logit: f32) -> f32 {
    1.0 / (1.0 + (-logit).exp())
}

fn candidates<'a>(
    logits: &'a [f32],
    config: &'a BeatConfig,
) -> impl Iterator<Item = (usize, f32)> + 'a {
    (0..logits.len()).filter_map(|index| {
        let start = index.saturating_sub(config.peak_half_width);
        let end = (index + config.peak_half_width + 1).min(logits.len());
        (logits[index] > config.peak_threshold
            && !logits[start..end]
                .iter()
                .any(|&value| value > logits[index]))
        .then_some((index, logits[index]))
    })
}

fn visit_deduplicated_peaks(
    mut peaks: impl Iterator<Item = (usize, f32)>,
    width: usize,
    mut visit: impl FnMut(Peak),
) {
    let Some((first, first_logit)) = peaks.next() else {
        return;
    };

    let mut p: f64 = first.as_();
    let mut logit = first_logit;
    let mut c = 1.0_f64;

    for (p2_usize, p2_logit) in peaks {
        let p2: f64 = p2_usize.as_();
        if p2 - p <= width.as_() {
            c += 1.0;
            p += (p2 - p) / c;
            logit = logit.max(p2_logit);
        } else {
            visit(Peak { logit, at: p });
            p = p2;
            logit = p2_logit;
            c = 1.0;
        }
    }
    visit(Peak { logit, at: p });
}

fn find_marks(logits: &[f32], config: &BeatConfig) -> Vec<BeatMark> {
    let mut marks: Vec<BeatMark> = Vec::new();
    visit_deduplicated_peaks(candidates(logits, config), config.dedup_width, |peak| {
        marks.push(peak.mark());
    });
    marks
}

#[cfg(test)]
fn find_peaks(logits: &[f32], config: &BeatConfig) -> Vec<Peak> {
    let mut peaks: Vec<Peak> = Vec::new();
    visit_deduplicated_peaks(candidates(logits, config), config.dedup_width, |peak| {
        peaks.push(peak);
    });
    peaks
}

#[cfg(test)]
fn deduplicate_peaks(peaks: &[(usize, f32)], width: usize) -> Vec<Peak> {
    let mut deduplicated: Vec<Peak> = Vec::new();
    visit_deduplicated_peaks(peaks.iter().copied(), width, |peak| {
        deduplicated.push(peak);
    });
    deduplicated
}

fn snap_downbeats_to_beats(beats: &[BeatMark], downbeats: &mut Vec<BeatMark>) {
    if beats.is_empty() || downbeats.is_empty() {
        return;
    }

    for down in downbeats.iter_mut() {
        let pos = beats.partition_point(|beat| beat.at < down.at);

        let best = match (pos.checked_sub(1), beats.get(pos)) {
            (Some(before), Some(after)) => {
                if (down.at - beats[before].at).abs() <= (after.at - down.at).abs() {
                    beats[before].at
                } else {
                    after.at
                }
            }
            (Some(before), None) => beats[before].at,
            (None, Some(after)) => after.at,
            (None, None) => continue,
        };

        down.at = best;
    }

    downbeats.sort_by(|a, b| a.at.total_cmp(&b.at));
    downbeats.dedup_by(|dropped, kept| {
        if dropped.at != kept.at {
            return false;
        }
        kept.confidence = kept.confidence.max(dropped.confidence);
        true
    });
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn at(peaks: &[Peak]) -> Vec<f64> {
        peaks.iter().map(|peak| peak.at).collect()
    }

    fn seconds(marks: &[BeatMark]) -> Vec<f32> {
        marks.iter().map(|mark| mark.at).collect()
    }

    fn flat(len: usize) -> Vec<f32> {
        vec![-5.0; len]
    }

    fn marks(at: &[f32]) -> Vec<BeatMark> {
        at.iter()
            .map(|&at| BeatMark {
                at,
                confidence: 0.5,
            })
            .collect()
    }

    #[kithara::test(native, flash(false))]
    fn a_stronger_peak_comes_back_more_confident() {
        let mut logits = flat(200);
        logits[50] = 0.5;
        logits[150] = 3.0;

        let picker = PeakPicker::new(BeatConfig::default());
        let (beats, _) = picker
            .decode(&logits, &flat(200))
            .expect("equal-length logits decode");

        assert_eq!(beats.len(), 2, "both peaks clear the threshold");
        assert!(
            beats[0].confidence < beats[1].confidence,
            "the stronger logit is the more confident mark: {beats:?}"
        );
        for beat in &beats {
            assert!(
                beat.confidence > 0.0 && beat.confidence < 1.0,
                "a probability is never a certainty: {beat:?}"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn the_default_threshold_is_an_even_chance() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6);
        assert!(sigmoid(-4.0) < 0.02);
        assert!(sigmoid(4.0) > 0.98);
    }

    #[kithara::test(native, flash(false))]
    fn merged_peaks_keep_their_strongest_evidence() {
        let merged = deduplicate_peaks(&[(10, 0.5), (11, 2.0)], 1);

        assert_eq!(merged.len(), 1, "the two frames are one beat");
        assert_eq!(merged[0].at, 10.5);
        assert_eq!(merged[0].logit, 2.0);
    }

    #[kithara::test(native, flash(false))]
    fn a_raised_threshold_drops_a_weak_peak() {
        let logits = [0.0, 0.0, 0.5, 1.0, 0.5, 0.0, 0.0];
        let strict = BeatConfig::builder().peak_threshold(1.5).build();

        assert_eq!(at(&find_peaks(&logits, &BeatConfig::default())), vec![3.0]);
        assert!(find_peaks(&logits, &strict).is_empty());
    }

    #[kithara::test(native, flash(false))]
    fn a_wider_window_suppresses_a_neighbour_the_default_keeps() {
        // 4 frames apart: each wins its own +-3 window, neither wins a +-4 one.
        let mut logits = vec![0.0; 10];
        logits[2] = 2.0;
        logits[6] = 1.0;
        let wide = BeatConfig::builder().peak_half_width(4).build();

        assert_eq!(
            at(&find_peaks(&logits, &BeatConfig::default())),
            vec![2.0, 6.0]
        );
        assert_eq!(at(&find_peaks(&logits, &wide)), vec![2.0]);
    }

    #[kithara::test(native, flash(false))]
    fn a_wider_dedup_merges_peaks_the_default_reports_apart() {
        let peaks = [(10, 1.0), (14, 1.0)];

        assert_eq!(at(&deduplicate_peaks(&peaks, 1)), vec![10.0, 14.0]);
        assert_eq!(at(&deduplicate_peaks(&peaks, 4)), vec![12.0]);
    }

    #[kithara::test(native, flash(false))]
    fn find_peaks_single_peak() {
        let logits = [0.0, 0.0, 0.5, 1.0, 0.5, 0.0, 0.0];
        let peaks = find_peaks(&logits, &BeatConfig::default());
        assert_eq!(at(&peaks), vec![3.0]);
    }

    #[kithara::test(native, flash(false))]
    fn find_peaks_below_threshold() {
        let logits = [-1.0, -0.5, -2.0, -0.1];
        let peaks = find_peaks(&logits, &BeatConfig::default());
        assert!(peaks.is_empty());
    }

    #[kithara::test(native, flash(false))]
    fn find_peaks_multiple_peaks() {
        // Two peaks separated by more than 3 frames.
        let mut logits = vec![0.0; 20];
        logits[3] = 2.0;
        logits[15] = 1.5;
        let peaks = find_peaks(&logits, &BeatConfig::default());
        assert_eq!(at(&peaks), vec![3.0, 15.0]);
    }

    #[kithara::test(native, flash(false))]
    fn find_peaks_window_suppresses_smaller_neighbour() {
        // A smaller positive value 3 frames from a larger one is not a peak.
        let mut logits = vec![0.0; 10];
        logits[4] = 2.0;
        logits[7] = 1.0;
        let peaks = find_peaks(&logits, &BeatConfig::default());
        assert_eq!(at(&peaks), vec![4.0]);
    }

    #[kithara::test(native, flash(false))]
    fn find_peaks_outside_window_both_survive() {
        // 4 frames apart: each is the max of its own ±3 window.
        let mut logits = vec![0.0; 10];
        logits[2] = 2.0;
        logits[6] = 1.0;
        let peaks = find_peaks(&logits, &BeatConfig::default());
        assert_eq!(at(&peaks), vec![2.0, 6.0]);
    }

    #[kithara::test(native, flash(false))]
    fn find_peaks_plateau_collapses_to_centre() {
        // Adjacent frames with equal positive values: both tie the max-pool,
        // dedup merges them to the plateau centre.
        let logits = [0.0, 1.0, 1.0, 0.0];
        let peaks = find_peaks(&logits, &BeatConfig::default());
        assert_eq!(peaks.len(), 1);
        assert_eq!(peaks[0].at, 1.5);
    }

    #[kithara::test(native, flash(false))]
    fn deduplicate_peaks_empty() {
        let peaks = deduplicate_peaks(&[], 1);
        assert!(peaks.is_empty());
    }

    #[kithara::test(native, flash(false))]
    fn deduplicate_peaks_no_adjacent() {
        let peaks = deduplicate_peaks(&[(5, 1.0), (10, 1.0), (20, 1.0)], 1);
        assert_eq!(at(&peaks), vec![5.0, 10.0, 20.0]);
    }

    #[kithara::test(native, flash(false))]
    fn deduplicate_peaks_merge() {
        // 10 and 11 merge (gap 1) to 10.5; 12 is 1.5 from the mean → new group.
        let peaks = deduplicate_peaks(&[(10, 1.0), (11, 1.0), (12, 1.0), (20, 1.0)], 1);
        assert_eq!(at(&peaks), vec![10.5, 12.0, 20.0]);

        // {10, 11, 11}: running mean 32/3, kept fractional.
        let peaks = deduplicate_peaks(&[(10, 1.0), (11, 1.0), (11, 1.0), (20, 1.0)], 1);
        assert_eq!(peaks.len(), 2);
        assert!((peaks[0].at - 32.0 / 3.0).abs() < 1e-9);
        assert_eq!(peaks[1].at, 20.0);
    }

    #[kithara::test(native, flash(false))]
    fn deduplicate_peaks_single() {
        let peaks = deduplicate_peaks(&[(42, 1.0)], 1);
        assert_eq!(at(&peaks), vec![42.0]);
    }

    #[kithara::test(native, flash(false))]
    fn snap_downbeats() {
        let beats = marks(&[1.0, 2.0, 3.0]);
        let mut downbeats = marks(&[1.1, 2.8]);
        snap_downbeats_to_beats(&beats, &mut downbeats);
        assert_eq!(seconds(&downbeats), vec![1.0, 3.0]);
    }

    #[kithara::test(native, flash(false))]
    fn snap_downbeats_dedup() {
        let beats = marks(&[1.0, 2.0, 3.0]);
        // Both downbeats snap to 2.0 and collapse to one, keeping the surer.
        let mut downbeats = vec![
            BeatMark {
                at: 1.8,
                confidence: 0.6,
            },
            BeatMark {
                at: 2.1,
                confidence: 0.9,
            },
        ];
        snap_downbeats_to_beats(&beats, &mut downbeats);
        assert_eq!(seconds(&downbeats), vec![2.0]);
        assert_eq!(downbeats[0].confidence, 0.9);
    }

    #[kithara::test(native, flash(false))]
    fn snap_downbeats_empty_beats() {
        let beats: Vec<BeatMark> = vec![];
        let mut downbeats = marks(&[1.0, 2.0]);
        snap_downbeats_to_beats(&beats, &mut downbeats);
        assert_eq!(seconds(&downbeats), vec![1.0, 2.0]);
    }

    #[kithara::test(native, flash(false))]
    fn snap_downbeats_empty_downbeats() {
        let beats = marks(&[1.0, 2.0]);
        let mut downbeats: Vec<BeatMark> = vec![];
        snap_downbeats_to_beats(&beats, &mut downbeats);
        assert!(downbeats.is_empty());
    }

    #[kithara::test(native, flash(false))]
    fn decode_full() {
        let mut beat_logits = vec![-5.0; 200];
        let mut downbeat_logits = vec![-5.0; 200];

        beat_logits[50] = 3.0;
        beat_logits[100] = 2.5;
        beat_logits[150] = 4.0;
        // Downbeat at frame 51 snaps to the beat at frame 50.
        downbeat_logits[51] = 2.0;

        let pp = PeakPicker::new(BeatConfig::default());
        let (beats, downbeats) = pp.decode(&beat_logits, &downbeat_logits).unwrap();

        assert_eq!(seconds(&beats), vec![1.0, 2.0, 3.0]);
        assert_eq!(seconds(&downbeats), vec![1.0]);
    }

    #[kithara::test(native, flash(false))]
    fn decode_empty_logits() {
        let pp = PeakPicker::new(BeatConfig::default());
        let (beats, downbeats) = pp.decode(&[], &[]).unwrap();
        assert!(beats.is_empty());
        assert!(downbeats.is_empty());
    }

    #[kithara::test(native, flash(false))]
    fn decode_mismatched_lengths() {
        let pp = PeakPicker::new(BeatConfig::default());
        let err = pp.decode(&[1.0, 2.0], &[1.0]);
        assert!(err.is_err());
    }
}
