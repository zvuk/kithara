use bon::Builder;
use num_traits::cast::ToPrimitive;

use super::{
    clean::{bar_gaps, classify_outliers, filter_close, find_stable_window, median},
    fit::{GridFitCtx, anchored_boundaries, build_segments},
};
use crate::{analysis::beat::detector::RawBeats, waveform::BeatGrid};

struct Consts;

impl Consts {
    const ALIGN_BARS: usize = 4;
    const BEATS_PER_BAR: f64 = 4.0;
    const MAX_BAR_RATIO: f64 = 2.0;
    const MEDIAN_TRUST_RATIO: f64 = 0.10;
    const MERGE_RATIO_EPS: f64 = 1e-3;
    const MIN_BAR_RATIO: f64 = 0.5;
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
    /// Drop a downbeat closer than this fraction of a nominal bar to its
    /// predecessor (double-detection filter).
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

/// Build a cleaned [`BeatGrid`] from raw detections: double-detection filter,
/// outlier classification, stable-window anchor, recursive bisection into
/// uniform-ratio segments. Positions convert from seconds to source frames at `sample_rate`.
pub(crate) fn build_grid(raw: &RawBeats, sample_rate: u32, params: &GridParams) -> BeatGrid {
    let sr = f64::from(sample_rate);
    let beats = clean_beats(&raw.beats, sr, params);

    let mut db: Vec<f64> = raw
        .downbeats
        .iter()
        .map(|&t| f64::from(t) * sr)
        .filter(|p| p.is_finite() && *p >= 0.0)
        .collect();
    db.sort_by(f64::total_cmp);
    if db.len() < Consts::MIN_DOWNBEATS {
        return BeatGrid::new(0.0, beats, positions_to_frames(&db), Vec::new());
    }

    // Nominal-bar seed for the double-detection filter and the trust band:
    // the global median gap is robust against scattered double detections.
    let nominal_seed = median(&bar_gaps(&db));
    let db = filter_close(db, params.min_gap_ratio * nominal_seed);
    let downbeats = positions_to_frames(&db);

    // Degraded mode (per plan): too short / no stable tempo region means no
    // trustworthy piecewise grid — report tempo only, no segments.
    let Some((anchor_idx, nominal_bar)) = find_stable_window(&db, nominal_seed, params) else {
        return BeatGrid::new(
            bar_to_bpm(median(&bar_gaps(&db)), sr),
            beats,
            downbeats,
            Vec::new(),
        );
    };

    let outliers = classify_outliers(&db, nominal_bar, params);
    let fit = GridFitCtx::new(&db, &outliers, sr, params);
    let boundaries = anchored_boundaries(&fit, anchor_idx);
    let segments = build_segments(&fit, &boundaries, nominal_bar);

    BeatGrid::new(bar_to_bpm(nominal_bar, sr), beats, downbeats, segments)
}

/// Beat marks in source frames, with the detector's double-detections
/// dropped — the same step 1 the downbeats already get.
///
/// Without it a doubled beat is a whole beat to everything downstream: the
/// beat map counts it, so the track beat under a playhead is wrong, and so is
/// the tempo anything reads off the marks. Measured on a real record, the raw
/// marks held gaps from 80 ms to 500 ms against a beat of 470.
fn clean_beats(secs: &[f32], sr: f64, params: &GridParams) -> Vec<u64> {
    let mut marks: Vec<f64> = secs
        .iter()
        .map(|&t| f64::from(t) * sr)
        .filter(|p| p.is_finite() && *p >= 0.0)
        .collect();
    marks.sort_by(f64::total_cmp);
    let nominal = median(&bar_gaps(&marks));
    positions_to_frames(&filter_close(marks, params.min_gap_ratio * nominal))
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

    /// A doubled beat is a whole beat to the beat map, so it moves the track
    /// beat under a playhead and the tempo anything reads off the marks. The
    /// downbeats have always been filtered for this; the beats are too.
    #[test]
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

    /// The filter drops what is too close, never what is merely uneven: a
    /// record that genuinely slows down keeps every beat it played.
    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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
        assert_eq!(grid.downbeats().len(), 9);
    }
}
