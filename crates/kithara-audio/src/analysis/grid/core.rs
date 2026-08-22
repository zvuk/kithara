use bon::Builder;
use num_traits::cast::ToPrimitive;

use super::{
    clean::{
        bar_gaps, classify_outliers, filter_close, filter_close_preferring, find_stable_window,
        median,
    },
    fit::{GridFitCtx, anchored_boundaries, build_segments},
    scratch::{GridPool, GridScratch},
};
use crate::{analysis::beat::detector::RawBeats, waveform::BeatGrid};

#[cfg(feature = "beat-nn")]
pub(crate) const GRID_SEMANTICS_TAG: &str = "grid_bpm_from_beats_v1";

struct Consts;

impl Consts {
    const ALIGN_BARS: usize = 4;
    const BEATS_PER_BAR: f64 = 4.0;
    const MAX_BAR_RATIO: f64 = 2.0;
    const MEDIAN_TRUST_RATIO: f64 = 0.10;
    const MERGE_RATIO_EPS: f64 = 1e-3;
    const MIN_BAR_RATIO: f64 = 0.5;
    /// Minimum mark gaps required to estimate tempo.
    const MIN_BEAT_GAPS: usize = 8;
    const MIN_MAP_BEATS: usize = 2;
    /// A bar gap needs two downbeats to measure.
    const MIN_DOWNBEATS: usize = 2;
    const MIN_GAP_RATIO: f64 = 0.7;
    const MIN_LEAF_BARS: usize = 8;
    const OUTLIER_RATIO: f64 = 0.04;
    const OUTLIER_WINDOW: usize = 4;
    const RESIDUAL_MS: f64 = 18.0;
    const SECS_PER_MIN: f64 = 60.0;
    const STABLE_WINDOW_BARS: usize = 16;
}

/// Grid-cleanup tuning.
#[derive(Builder, Debug, Clone, PartialEq)]
pub(crate) struct GridParams {
    #[builder(default = Consts::MAX_BAR_RATIO)]
    pub(crate) max_bar_ratio: f64,
    /// Stable window median must lie within this fraction of nominal.
    #[builder(default = Consts::MEDIAN_TRUST_RATIO)]
    pub(crate) median_trust_ratio: f64,
    /// Merge adjacent leaves whose ratio corrections agree within this
    /// epsilon — collinear halves around the anchor collapse to one segment.
    #[builder(default = Consts::MERGE_RATIO_EPS)]
    pub(crate) merge_ratio_eps: f64,
    /// Hard sanity bounds on a bar length, as fractions of the nominal bar.
    #[builder(default = Consts::MIN_BAR_RATIO)]
    pub(crate) min_bar_ratio: f64,
    /// Drop a detector mark closer than this fraction of its nominal interval.
    #[builder(default = Consts::MIN_GAP_RATIO)]
    pub(crate) min_gap_ratio: f64,
    /// Outlier threshold vs the neighbour-window median bar factor.
    #[builder(default = Consts::OUTLIER_RATIO)]
    pub(crate) outlier_ratio: f64,
    /// Bisection leaf fit tolerance: worst bar residual, milliseconds.
    #[builder(default = Consts::RESIDUAL_MS)]
    pub(crate) residual_ms: f64,
    /// Snap bisection split points to multiples of this many bars.
    #[builder(default = Consts::ALIGN_BARS)]
    pub(crate) align_bars: usize,
    /// Minimum segment length in bars.
    #[builder(default = Consts::MIN_LEAF_BARS)]
    pub(crate) min_leaf_bars: usize,
    /// Neighbour median window (bars each side) for outlier classification.
    #[builder(default = Consts::OUTLIER_WINDOW)]
    pub(crate) outlier_window: usize,
    /// Sliding window length (bars) for the stable-tempo anchor search.
    #[builder(default = Consts::STABLE_WINDOW_BARS)]
    pub(crate) stable_window_bars: usize,
}

impl Default for GridParams {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Builds a cleaned [`BeatGrid`] in source frames, preferring beat marks for tempo.
pub(crate) fn build_grid(
    raw: &RawBeats,
    sample_rate: u32,
    params: &GridParams,
    pool: &GridPool,
) -> BeatGrid {
    pool.with(raw.beats.len(), raw.downbeats.len(), |scratch| {
        build_grid_with(raw, sample_rate, params, scratch)
    })
}

fn build_grid_with(
    raw: &RawBeats,
    sample_rate: u32,
    params: &GridParams,
    scratch: &mut GridScratch,
) -> BeatGrid {
    let sr = f64::from(sample_rate);
    scratch.positions.clear();
    scratch.positions.extend(
        raw.downbeats
            .iter()
            .map(|&time| f64::from(time) * sr)
            .filter(|position| position.is_finite() && *position >= 0.0),
    );
    scratch.positions.sort_unstable_by(f64::total_cmp);
    bar_gaps(&scratch.positions, &mut scratch.gaps);
    let nominal_seed = median(&scratch.gaps, &mut scratch.sorted);
    filter_close(&mut scratch.positions, params.min_gap_ratio * nominal_seed);
    bar_gaps(&scratch.positions, &mut scratch.gaps);
    let downbeat_bpm = bar_to_bpm(median(&scratch.gaps, &mut scratch.sorted), sr);

    let beats = clean_beats(
        &raw.beats,
        &scratch.positions,
        sr,
        params,
        &mut scratch.marks,
        &mut scratch.gaps,
        &mut scratch.sorted,
    );
    let marks_bpm = beats_bpm(&beats, sr, &mut scratch.gaps, &mut scratch.sorted);
    if beats.len() >= Consts::MIN_MAP_BEATS {
        scratch.positions.retain(|position| {
            position
                .round()
                .to_u64()
                .is_some_and(|frame| beats.binary_search(&frame).is_ok())
        });
    }

    if scratch.positions.len() < Consts::MIN_DOWNBEATS {
        return BeatGrid::new(
            marks_bpm.unwrap_or(downbeat_bpm),
            beats,
            positions_to_frames(&scratch.positions),
            Vec::new(),
        );
    }
    bar_gaps(&scratch.positions, &mut scratch.gaps);
    let nominal_seed = median(&scratch.gaps, &mut scratch.sorted);
    let downbeats = positions_to_frames(&scratch.positions);

    // Degraded mode (per plan): too short / no stable tempo region means no
    // trustworthy piecewise grid — report tempo only, no segments.
    let Some((anchor_idx, nominal_bar)) = find_stable_window(
        &scratch.positions,
        nominal_seed,
        params,
        &mut scratch.gaps,
        &mut scratch.sorted,
    ) else {
        return BeatGrid::new(
            marks_bpm.unwrap_or(downbeat_bpm),
            beats,
            downbeats,
            Vec::new(),
        );
    };

    classify_outliers(
        &scratch.positions,
        nominal_bar,
        params,
        &mut scratch.outliers,
        &mut scratch.neighbors,
        &mut scratch.sorted,
    );
    let fit = GridFitCtx::new(&scratch.positions, &scratch.outliers, sr, params);
    anchored_boundaries(&fit, anchor_idx, &mut scratch.boundaries);
    let segments = build_segments(&fit, &scratch.boundaries, nominal_bar, &mut scratch.spans);

    BeatGrid::new(
        marks_bpm.unwrap_or_else(|| bar_to_bpm(nominal_bar, sr)),
        beats,
        downbeats,
        segments,
    )
}

/// Converts beat marks to source frames and removes close double detections.
fn clean_beats(
    secs: &[f32],
    downbeats: &[f64],
    sr: f64,
    params: &GridParams,
    marks: &mut Vec<f64>,
    gaps: &mut Vec<f64>,
    sorted: &mut Vec<f64>,
) -> Vec<u64> {
    marks.clear();
    marks.extend(
        secs.iter()
            .map(|&time| f64::from(time) * sr)
            .filter(|position| position.is_finite() && *position >= 0.0),
    );
    marks.sort_unstable_by(f64::total_cmp);
    bar_gaps(marks, gaps);
    let nominal = median(gaps, sorted);
    filter_close_preferring(marks, downbeats, params.min_gap_ratio * nominal);
    positions_to_frames(marks)
}

/// Estimates tempo by weighting each gap by the beat spans it covers.
fn beats_bpm(beats: &[u64], sr: f64, gaps: &mut Vec<f64>, sorted: &mut Vec<f64>) -> Option<f64> {
    gaps.clear();
    gaps.extend(beats.windows(2).filter_map(|window| {
        window[1]
            .checked_sub(window[0])
            .and_then(|gap| gap.to_f64())
            .filter(|gap| *gap > 0.0)
    }));
    if gaps.len() < Consts::MIN_BEAT_GAPS {
        return None;
    }

    let mut beat = median(gaps, sorted);
    for _ in 0..2 {
        if beat <= 0.0 {
            return None;
        }
        let mut span = 0.0;
        let mut count = 0.0;
        for &gap in gaps.iter() {
            let k = (gap / beat).round();
            if k > 0.0 {
                span += gap;
                count += k;
            }
        }
        if count == 0.0 {
            return None;
        }
        beat = span / count;
    }
    (beat > 0.0).then(|| Consts::SECS_PER_MIN * sr / beat)
}

fn positions_to_frames(positions: &[f64]) -> Vec<u64> {
    positions
        .iter()
        .filter_map(|p| p.round().to_u64())
        .collect()
}

/// 4/4 bars: bpm = beats-per-bar (4) × 60 / bar-seconds.
fn bar_to_bpm(bar_samples: f64, sr: f64) -> f64 {
    if bar_samples > 0.0 {
        Consts::BEATS_PER_BAR * Consts::SECS_PER_MIN * sr / bar_samples
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    struct Consts;

    impl Consts {
        const SR: u32 = 44_100;
        const TOL_100MS: u64 = 4_410;
        /// 0.02 s and 0.1 s at `SR`, in frames.
        const TOL_20MS: u64 = 882;
    }

    fn raw(downbeats: Vec<f32>) -> RawBeats {
        RawBeats {
            downbeats,
            beats: Vec::new(),
        }
    }

    fn build_grid(raw: &RawBeats, sample_rate: u32, params: &GridParams) -> BeatGrid {
        super::build_grid(raw, sample_rate, params, &GridPool::default())
    }

    #[kithara::test(native, flash(false))]
    fn a_doubled_beat_is_dropped_like_a_doubled_downbeat() {
        let beat = 0.5f32;
        let mut beats: Vec<f32> = (0..64u16).map(|i| f32::from(i) * beat).collect();
        let doubles: Vec<f32> = beats.iter().step_by(8).map(|t| t + beat * 0.1).collect();
        beats.extend(doubles);
        beats.sort_by(f32::total_cmp);

        let grid = build_grid(
            &RawBeats {
                downbeats: steady(0.0, 2.0, 16),
                beats,
            },
            Consts::SR,
            &GridParams::default(),
        );

        assert_eq!(
            grid.beats().len(),
            64,
            "the doubled strikes must be dropped"
        );
    }

    #[kithara::test(native, flash(false))]
    fn cleaning_keeps_every_downbeat_on_a_retained_beat() {
        let beat = 0.5f32;
        let mut beats: Vec<f32> = (0..64u16).map(|i| f32::from(i) * beat).collect();
        beats.insert(1, beat * 0.1);
        let mut downbeats = steady(0.0, 2.0, 16);
        downbeats[0] = beat * 0.1;

        let grid = build_grid(
            &RawBeats { beats, downbeats },
            Consts::SR,
            &GridParams::default(),
        );

        assert_eq!(grid.downbeats().first(), grid.beats().first());
        assert!(
            grid.downbeats()
                .iter()
                .all(|downbeat| grid.beats().binary_search(downbeat).is_ok()),
            "every downbeat must remain a beat after cleaning"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_downbeat_wins_a_close_beat_collision() {
        let mut beats = steady(0.0, 0.5, 12);
        beats.push(1.9);
        beats.sort_by(f32::total_cmp);

        let grid = build_grid(
            &RawBeats {
                beats,
                downbeats: vec![0.0, 2.0, 4.0, 6.0],
            },
            100,
            &GridParams::default(),
        );

        assert!(grid.beats().binary_search(&200).is_ok());
        assert!(grid.beats().binary_search(&190).is_err());
    }

    #[kithara::test(native, flash(false))]
    fn unmatched_downbeat_is_omitted_from_a_map_capable_grid() {
        let grid = build_grid(
            &RawBeats {
                beats: steady(0.0, 0.5, 12),
                downbeats: vec![0.0, 2.25, 4.0, 6.0],
            },
            100,
            &GridParams::default(),
        );

        assert!(grid.downbeats().binary_search(&225).is_err());
        assert!(
            grid.downbeats()
                .iter()
                .all(|downbeat| grid.beats().binary_search(downbeat).is_ok())
        );
    }

    #[kithara::test(native, flash(false))]
    fn an_evenly_spaced_beat_track_survives_the_filter_whole() {
        let beats: Vec<f32> = (0..64u16).map(|i| f32::from(i) * 0.5).collect();
        let grid = build_grid(
            &RawBeats {
                downbeats: steady(0.0, 2.0, 16),
                beats,
            },
            Consts::SR,
            &GridParams::default(),
        );

        assert_eq!(grid.beats().len(), 64, "no clean beat may be dropped");
    }

    #[kithara::test(native, flash(false))]
    fn quantized_gaps_average_out_instead_of_latching_to_the_grid() {
        // 37 gaps of 0.48 s and 25 of 0.46 s: a 127.14 BPM record seen
        // through a 20 ms grid. The single-gap median latches onto 0.48 s.
        let mut beats = vec![0.0f32];
        let mut t = 0.0f32;
        for i in 0..62usize {
            t += if i < 50 && i % 2 == 1 { 0.46 } else { 0.48 };
            beats.push(t);
        }

        let grid = build_grid(
            &RawBeats {
                downbeats: Vec::new(),
                beats,
            },
            Consts::SR,
            &GridParams::default(),
        );

        assert!(
            (grid.bpm() - 127.136).abs() < 0.1,
            "tempo must match the span the marks actually cover, got {}",
            grid.bpm()
        );
        assert!(
            (grid.bpm() - 125.0).abs() > 0.5,
            "tempo must not latch onto the 0.48 s grid step, got {}",
            grid.bpm()
        );
    }

    #[kithara::test(native, flash(false))]
    fn skipped_marks_do_not_bend_the_tempo() {
        let beats: Vec<f32> = (0..64u16)
            .filter(|i| i % 7 != 6)
            .map(|i| f32::from(i) * 0.5)
            .collect();

        let grid = build_grid(
            &RawBeats {
                downbeats: Vec::new(),
                beats,
            },
            Consts::SR,
            &GridParams::default(),
        );

        assert!(
            (grid.bpm() - 120.0).abs() < 0.1,
            "tempo must survive the holes, got {}",
            grid.bpm()
        );
        // 54 gaps over 31.5 s: a plain average over the gaps reads 102.86.
        assert!(
            (grid.bpm() - 102.86).abs() > 5.0,
            "a doubled gap must count as two beats, not one, got {}",
            grid.bpm()
        );
    }

    #[kithara::test(native, flash(false))]
    fn marks_out_vote_downbeats_that_strike_every_beat() {
        let beats: Vec<f32> = (0..64u16).map(|i| f32::from(i) * 0.4).collect();
        let grid = build_grid(
            &RawBeats {
                downbeats: beats.clone(),
                beats,
            },
            Consts::SR,
            &GridParams::default(),
        );

        assert!(
            (grid.bpm() - 150.0).abs() < 0.2,
            "tempo must read off the marks, got {}",
            grid.bpm()
        );
        assert!(
            (600.0 - grid.bpm()).abs() > 100.0,
            "the four-beat bar must not quadruple the tempo, got {}",
            grid.bpm()
        );
    }

    /// `bars + 1` downbeats starting at `start`, one every `bar` seconds.
    fn steady(start: f32, bar: f32, bars: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(bars + 1);
        let mut t = start;
        for _ in 0..=bars {
            out.push(t);
            t += bar;
        }
        out
    }

    fn nearest_downbeat_idx(downbeats: &[u64], frame: u64) -> usize {
        let mut best = 0;
        let mut best_d = u64::MAX;
        for (i, &d) in downbeats.iter().enumerate() {
            let dist = d.abs_diff(frame);
            if dist < best_d {
                best_d = dist;
                best = i;
            }
        }
        best
    }

    #[kithara::test(native, flash(false))]
    fn clean_track_is_one_on_grid_segment() {
        let grid = build_grid(
            &raw(steady(1.0, 2.0, 64)),
            Consts::SR,
            &GridParams::default(),
        );

        assert_eq!(grid.downbeats().len(), 65, "all clean downbeats kept");
        assert_eq!(grid.downbeats()[0], 44_100, "seconds convert to frames");
        assert!(
            (grid.bpm() - 120.0).abs() < 0.2,
            "bpm from 2 s bars, got {}",
            grid.bpm()
        );
        assert_eq!(grid.segments().len(), 1, "clean track is a single leaf");
        let seg = grid.segments()[0];
        assert!(
            (seg.ratio_correction() - 1.0).abs() < 2e-3,
            "on-grid leaf needs no correction, got {}",
            seg.ratio_correction()
        );
        let first = 44_100; // 1.0 s
        let last = 129 * 44_100; // 1.0 s + 64 bars of 2.0 s
        assert!(seg.start_frame().abs_diff(first) < Consts::TOL_20MS);
        assert!(seg.end_frame().abs_diff(last) < Consts::TOL_20MS);
    }

    #[kithara::test(native, flash(false))]
    fn drifting_track_splits_into_phrase_aligned_segments() {
        // 32 bars at 2.00 s, then 32 bars at 2.06 s (3 % drift, not outlier).
        let mut db = Vec::new();
        let mut t = 1.0f32;
        for _ in 0..32 {
            db.push(t);
            t += 2.0;
        }
        for _ in 0..32 {
            db.push(t);
            t += 2.06;
        }
        db.push(t);
        let grid = build_grid(&raw(db), Consts::SR, &GridParams::default());

        assert!(
            grid.segments().len() >= 2,
            "tempo change must split the grid, got {} segments",
            grid.segments().len()
        );
        let corrections: Vec<f64> = grid
            .segments()
            .iter()
            .map(crate::region::GridSegment::ratio_correction)
            .collect();
        let min = corrections.iter().copied().fold(f64::INFINITY, f64::min);
        let max = corrections
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            ((max / min) - 2.06 / 2.0).abs() < 5e-3,
            "correction spread must match the 3% tempo drift, got {}",
            max / min
        );

        for pair in grid.segments().windows(2) {
            let boundary = pair[0].end_frame();
            let idx = nearest_downbeat_idx(grid.downbeats(), boundary);
            let near = grid.downbeats()[idx].abs_diff(boundary);
            assert!(
                near < Consts::TOL_100MS,
                "segment boundary must land on a downbeat"
            );
            assert_eq!(idx % 4, 0, "boundary bar {idx} must sit on a 4-bar phrase");
        }
        for seg in grid.segments() {
            let bars = grid
                .downbeats()
                .iter()
                .filter(|&&d| d >= seg.start_frame() && d < seg.end_frame())
                .count();
            assert!(bars >= 8, "leaf shorter than min 8 bars: {bars}");
        }
    }

    #[kithara::test(native, flash(false))]
    fn double_detections_are_filtered() {
        // Clean 64-bar track plus spurious half-bar downbeats (halving error).
        let mut db = steady(1.0, 2.0, 64);
        let extras: Vec<f32> = [10usize, 20, 30].iter().map(|&i| db[i] + 1.0).collect();
        db.extend(extras);
        db.sort_by(f32::total_cmp);
        let grid = build_grid(&raw(db), Consts::SR, &GridParams::default());

        assert_eq!(
            grid.downbeats().len(),
            65,
            "spurious half-bar downbeats must be dropped"
        );
        assert_eq!(grid.segments().len(), 1);
        assert!((grid.segments()[0].ratio_correction() - 1.0).abs() < 2e-3);
        assert!((grid.bpm() - 120.0).abs() < 0.2);
    }

    #[kithara::test(native, flash(false))]
    fn fade_out_garbage_does_not_bend_the_grid() {
        // Clean 64 bars, then a sparse fade-out tail with bogus gaps.
        let mut db = steady(1.0, 2.0, 64);
        for t in [132.2f32, 135.0, 138.4, 141.0] {
            db.push(t);
        }
        let grid = build_grid(&raw(db), Consts::SR, &GridParams::default());

        assert!((grid.bpm() - 120.0).abs() < 0.5, "bpm {}", grid.bpm());
        for seg in grid.segments() {
            assert!(
                (seg.ratio_correction() - 1.0).abs() < 0.05,
                "outlier tail must not bend any segment, got {}",
                seg.ratio_correction()
            );
        }
        assert!(!grid.segments().is_empty());
    }

    #[kithara::test(native, flash(false))]
    fn short_track_yields_tempo_without_segments() {
        let beats = vec![0.5f32, 1.0, 1.5];
        let grid = build_grid(
            &RawBeats {
                beats,
                downbeats: steady(1.0, 2.0, 8),
            },
            Consts::SR,
            &GridParams::default(),
        );

        assert!((grid.bpm() - 120.0).abs() < 0.2, "bpm {}", grid.bpm());
        assert!(
            grid.segments().is_empty(),
            "no stable window means no trustworthy segments"
        );
        assert_eq!(grid.beats(), [22_050, 44_100, 66_150]);
        assert_eq!(grid.downbeats(), [44_100]);
        assert!(
            grid.downbeats()
                .iter()
                .all(|downbeat| grid.beats().binary_search(downbeat).is_ok())
        );
    }

    #[kithara::test(native, flash(false))]
    fn downbeat_only_short_track_remains_a_degraded_tempo_grid() {
        let grid = build_grid(
            &raw(steady(1.0, 2.0, 8)),
            Consts::SR,
            &GridParams::default(),
        );

        assert!((grid.bpm() - 120.0).abs() < 0.2, "bpm {}", grid.bpm());
        assert!(grid.segments().is_empty());
        assert!(grid.beats().is_empty());
        assert_eq!(grid.downbeats().len(), 9);
    }
}
