use bon::Builder;
#[cfg(test)]
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_bufpool::{PoolError, SampleBuffer};
use num_traits::cast::ToPrimitive;

use super::{
    clean::{
        bar_gaps, classify_outliers, filter_close, filter_close_preferring, find_stable_window,
        median,
    },
    fit::{GridFitCtx, build_segments},
    scratch::{GridBuffers, fill, retain},
};
use crate::{
    BeatArtifact,
    artifact::MarkedBeat,
    beat::detector::{BeatMark, RawBeats},
};

#[cfg(any(feature = "beat-nn", feature = "beat-dsp"))]
pub(crate) const GRID_SEMANTICS_TAG: &str = "grid_bpm_from_beats_v2";

struct Consts;

impl Consts {
    const ALIGN_BARS: usize = 4;
    const BEATS_PER_BAR: f64 = 4.0;
    const MAX_BAR_RATIO: f64 = 2.0;
    const MEDIAN_TRUST_RATIO: f64 = 0.10;
    const MERGE_RATIO_EPS: f64 = 1e-3;
    const MIN_BAR_RATIO: f64 = 0.5;
    const MIN_BEAT_GAPS: usize = 8;
    const MIN_DOWNBEATS: usize = 2;
    const MIN_GAP_RATIO: f64 = 0.7;
    const MIN_LEAF_BARS: usize = 8;
    const MIN_MAP_BEATS: usize = 2;
    const OUTLIER_RATIO: f64 = 0.04;
    const OUTLIER_WINDOW: usize = 4;
    const RESIDUAL_MS: f64 = 18.0;
    const SECS_PER_MIN: f64 = 60.0;
    const STABLE_WINDOW_BARS: usize = 16;
}

#[derive(Builder, Debug, Clone, PartialEq)]
pub(crate) struct GridParams {
    #[builder(default = Consts::MAX_BAR_RATIO)]
    pub(crate) max_bar_ratio: f64,
    #[builder(default = Consts::MEDIAN_TRUST_RATIO)]
    pub(crate) median_trust_ratio: f64,
    #[builder(default = Consts::MERGE_RATIO_EPS)]
    pub(crate) merge_ratio_eps: f64,
    #[builder(default = Consts::MIN_BAR_RATIO)]
    pub(crate) min_bar_ratio: f64,
    #[builder(default = Consts::MIN_GAP_RATIO)]
    pub(crate) min_gap_ratio: f64,
    #[builder(default = Consts::OUTLIER_RATIO)]
    pub(crate) outlier_ratio: f64,
    #[builder(default = Consts::RESIDUAL_MS)]
    pub(crate) residual_ms: f64,
    #[builder(default = Consts::ALIGN_BARS)]
    pub(crate) align_bars: usize,
    #[builder(default = Consts::MIN_LEAF_BARS)]
    pub(crate) min_leaf_bars: usize,
    #[builder(default = Consts::OUTLIER_WINDOW)]
    pub(crate) outlier_window: usize,
    #[builder(default = Consts::STABLE_WINDOW_BARS)]
    pub(crate) stable_window_bars: usize,
}

impl Default for GridParams {
    fn default() -> Self {
        Self::builder().build()
    }
}

#[cfg(test)]
fn build_grid<S>(
    raw: &RawBeats,
    sample_rate: u32,
    params: &GridParams,
    pools: &PoolRegion<S>,
) -> Result<BeatArtifact, PoolError>
where
    S: HasPool<f32>,
{
    let mut buffers = GridBuffers::new(pools);
    build_grid_with(raw, sample_rate, params, &mut buffers)
}

pub(crate) fn build_grid_with(
    raw: &RawBeats,
    sample_rate: u32,
    params: &GridParams,
    buffers: &mut GridBuffers,
) -> Result<BeatArtifact, PoolError> {
    let sr = f64::from(sample_rate);
    debug_assert!(
        is_sorted(&raw.beats) && is_sorted(&raw.downbeats),
        "build_grid needs detector marks sorted by position"
    );
    fill(
        &mut buffers.positions,
        raw.downbeats.iter().map(|mark| mark.at),
    )?;
    retain(&mut buffers.positions, |time| {
        time.is_finite() && time >= 0.0
    });
    buffers.positions.sort_unstable_by(f32::total_cmp);
    bar_gaps(&buffers.positions, &mut buffers.gaps)?;
    let nominal_seed = median(&buffers.gaps, &mut buffers.sorted)?;
    filter_close(&mut buffers.positions, params.min_gap_ratio * nominal_seed);
    bar_gaps(&buffers.positions, &mut buffers.gaps)?;
    let downbeat_bpm = bar_to_bpm(median(&buffers.gaps, &mut buffers.sorted)?);

    clean_beats(
        &raw.beats,
        &buffers.positions,
        params,
        &mut buffers.marks,
        &mut buffers.gaps,
        &mut buffers.sorted,
    )?;
    let marks_bpm = beats_bpm(&buffers.marks, &mut buffers.gaps, &mut buffers.sorted)?;
    let beats = marks_to_frames(&buffers.marks, &raw.beats, sr);
    if beats.len() >= Consts::MIN_MAP_BEATS {
        retain(&mut buffers.positions, |position| {
            position_to_frame(position, sr).is_some_and(|frame| {
                beats
                    .binary_search_by(|(candidate, _)| candidate.cmp(&frame))
                    .is_ok()
            })
        });
    }

    if buffers.positions.len() < Consts::MIN_DOWNBEATS {
        return Ok(BeatArtifact::new(
            marks_bpm.unwrap_or(downbeat_bpm),
            beats,
            marks_to_frames(&buffers.positions, &raw.downbeats, sr),
        ));
    }
    bar_gaps(&buffers.positions, &mut buffers.gaps)?;
    let nominal_seed = median(&buffers.gaps, &mut buffers.sorted)?;
    let downbeats = marks_to_frames(&buffers.positions, &raw.downbeats, sr);

    let Some((anchor_idx, nominal_bar)) = find_stable_window(
        &buffers.positions,
        nominal_seed,
        params,
        &mut buffers.gaps,
        &mut buffers.sorted,
    )?
    else {
        return Ok(BeatArtifact::new(
            marks_bpm.unwrap_or(downbeat_bpm),
            beats,
            downbeats,
        ));
    };

    classify_outliers(
        &buffers.positions,
        nominal_bar,
        params,
        &mut buffers.outliers,
        &mut buffers.neighbors,
        &mut buffers.sorted,
    )?;
    let fit = GridFitCtx::new(&buffers.positions, &buffers.outliers, sr, params);
    let segments = build_segments(&fit, anchor_idx, nominal_bar);

    Ok(BeatArtifact::with_regions(
        marks_bpm.unwrap_or_else(|| bar_to_bpm(nominal_bar)),
        beats,
        downbeats,
        segments,
    ))
}

fn clean_beats(
    secs: &[BeatMark],
    downbeats: &[f32],
    params: &GridParams,
    marks: &mut SampleBuffer,
    gaps: &mut SampleBuffer,
    sorted: &mut SampleBuffer,
) -> Result<(), PoolError> {
    fill(marks, secs.iter().map(|mark| mark.at))?;
    retain(marks, |time| time.is_finite() && time >= 0.0);
    marks.sort_unstable_by(f32::total_cmp);
    bar_gaps(marks, gaps)?;
    let nominal = median(gaps, sorted)?;
    filter_close_preferring(marks, downbeats, params.min_gap_ratio * nominal);
    Ok(())
}

fn beats_bpm(
    beats: &[f32],
    gaps: &mut SampleBuffer,
    sorted: &mut SampleBuffer,
) -> Result<Option<f64>, PoolError> {
    bar_gaps(beats, gaps)?;
    retain(gaps, |gap| gap > 0.0);
    if gaps.len() < Consts::MIN_BEAT_GAPS {
        return Ok(None);
    }

    let mut beat = median(gaps, sorted)?;
    for _ in 0..2 {
        if beat <= 0.0 {
            return Ok(None);
        }
        let mut span = 0.0;
        let mut count = 0.0;
        for &gap in gaps.iter() {
            let gap = f64::from(gap);
            let k = (gap / beat).round();
            if k > 0.0 {
                span += gap;
                count += k;
            }
        }
        if count == 0.0 {
            return Ok(None);
        }
        beat = span / count;
    }
    Ok((beat > 0.0).then(|| Consts::SECS_PER_MIN / beat))
}

fn marks_to_frames(positions: &[f32], detected: &[BeatMark], sample_rate: f64) -> Vec<MarkedBeat> {
    let mut out: Vec<MarkedBeat> = Vec::with_capacity(positions.len());
    for &position in positions {
        let Some(frame) = position_to_frame(position, sample_rate) else {
            continue;
        };
        let confidence = confidence_at(detected, position);
        debug_assert!(
            confidence.is_some(),
            "a cleaned position must be one the detector reported: {position}"
        );
        out.push((frame, confidence));
    }
    out
}

fn confidence_at(detected: &[BeatMark], position: f32) -> Option<f32> {
    detected
        .binary_search_by(|mark| mark.at.total_cmp(&position))
        .ok()
        .and_then(|index| detected.get(index))
        .map(|mark| mark.confidence)
}

fn is_sorted(marks: &[BeatMark]) -> bool {
    marks.windows(2).all(|pair| pair[0].at <= pair[1].at)
}

fn position_to_frame(position: f32, sample_rate: f64) -> Option<u64> {
    (f64::from(position) * sample_rate).round().to_u64()
}

fn bar_to_bpm(bar_seconds: f64) -> f64 {
    if bar_seconds > 0.0 {
        Consts::BEATS_PER_BAR * Consts::SECS_PER_MIN / bar_seconds
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::pools;

    struct Consts;

    impl Consts {
        const SR: u32 = 44_100;
        const TOL_100MS: u64 = 4_410;
        const TOL_20MS: u64 = 882;
    }

    fn marks(times: Vec<f32>) -> Vec<BeatMark> {
        times.into_iter().map(BeatMark::at).collect()
    }

    fn raw(downbeats: Vec<f32>) -> RawBeats {
        RawBeats {
            downbeats: marks(downbeats),
            beats: Vec::new(),
        }
    }

    #[kithara::test(native, flash(false))]
    fn cleaning_leaves_every_survivor_with_its_own_confidence() {
        // A beat every half second, each with a confidence of its own, plus a
        // straggler at 1.9 that cleaning drops for colliding with 2.0.
        let mut beats: Vec<BeatMark> = (0..13u8)
            .map(|n| {
                let t = f32::from(n) * 0.5;
                BeatMark {
                    at: t,
                    confidence: 0.2 + f32::from(n) * 0.05,
                }
            })
            .collect();
        beats.push(BeatMark {
            at: 1.9,
            confidence: 0.99,
        });
        beats.sort_by(|a, b| a.at.total_cmp(&b.at));
        let detected = beats.clone();

        let grid = build_grid(
            &RawBeats {
                beats,
                downbeats: marks(vec![0.0, 2.0, 4.0, 6.0]),
            },
            100,
            &GridParams::default(),
        );

        assert!(
            grid.beats().binary_search(&190).is_err(),
            "the straggler is dropped by cleaning"
        );
        assert!(!grid.beats().is_empty(), "the grid keeps the rest");

        for (&frame, &confidence) in grid.beats().iter().zip(grid.beat_confidence()) {
            let at = frame.to_f32().unwrap_or(0.0) / 100.0;
            let want = detected
                .iter()
                .find(|mark| (mark.at - at).abs() < 1e-4)
                .map(|mark| mark.confidence);
            assert_eq!(
                confidence, want,
                "the survivor at {at} kept the confidence it was detected with"
            );
        }
    }

    fn build_grid(raw: &RawBeats, sample_rate: u32, params: &GridParams) -> BeatArtifact {
        super::build_grid(raw, sample_rate, params, &pools())
            .expect("grid scratch fits the PCM pool budget")
    }

    #[kithara::test(native, flash(false))]
    fn injected_region_reuses_grid_buffers() {
        let pools = pools();
        let raw = RawBeats {
            beats: marks(steady(0.0, 0.5, 128)),
            downbeats: marks(steady(0.0, 2.0, 64)),
        };

        super::build_grid(&raw, Consts::SR, &GridParams::default(), &pools)
            .expect("first grid fits the PCM pool budget");
        let allocated = pools.stats().allocated_bytes;

        super::build_grid(&raw, Consts::SR, &GridParams::default(), &pools)
            .expect("second grid fits the PCM pool budget");

        assert_eq!(pools.stats().allocated_bytes, allocated);
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
                downbeats: marks(steady(0.0, 2.0, 16)),
                beats: marks(beats),
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
            &RawBeats {
                beats: marks(beats),
                downbeats: marks(downbeats),
            },
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
                beats: marks(beats),
                downbeats: marks(vec![0.0, 2.0, 4.0, 6.0]),
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
                beats: marks(steady(0.0, 0.5, 12)),
                downbeats: marks(vec![0.0, 2.25, 4.0, 6.0]),
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
                downbeats: marks(steady(0.0, 2.0, 16)),
                beats: marks(beats),
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
                beats: marks(beats),
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
                beats: marks(beats),
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
                downbeats: marks(beats.clone()),
                beats: marks(beats),
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
        assert_eq!(grid.regions().len(), 1, "clean track is a single leaf");
        let seg = grid.regions()[0];
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
            grid.regions().len() >= 2,
            "tempo change must split the grid, got {} segments",
            grid.regions().len()
        );
        let corrections: Vec<f64> = grid
            .regions()
            .iter()
            .map(crate::artifact::FitRegion::ratio_correction)
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

        for pair in grid.regions().windows(2) {
            let boundary = pair[0].end_frame();
            let idx = nearest_downbeat_idx(grid.downbeats(), boundary);
            let near = grid.downbeats()[idx].abs_diff(boundary);
            assert!(
                near < Consts::TOL_100MS,
                "segment boundary must land on a downbeat"
            );
            assert_eq!(idx % 4, 0, "boundary bar {idx} must sit on a 4-bar phrase");
        }
        for seg in grid.regions() {
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
        assert_eq!(grid.regions().len(), 1);
        assert!((grid.regions()[0].ratio_correction() - 1.0).abs() < 2e-3);
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
        for seg in grid.regions() {
            assert!(
                (seg.ratio_correction() - 1.0).abs() < 0.05,
                "outlier tail must not bend any segment, got {}",
                seg.ratio_correction()
            );
        }
        assert!(!grid.regions().is_empty());
    }

    #[kithara::test(native, flash(false))]
    fn short_track_yields_tempo_without_segments() {
        let beats = vec![0.5f32, 1.0, 1.5];
        let grid = build_grid(
            &RawBeats {
                beats: marks(beats),
                downbeats: marks(steady(1.0, 2.0, 8)),
            },
            Consts::SR,
            &GridParams::default(),
        );

        assert!((grid.bpm() - 120.0).abs() < 0.2, "bpm {}", grid.bpm());
        assert!(
            grid.regions().is_empty(),
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
        assert!(grid.regions().is_empty());
        assert!(grid.beats().is_empty());
        assert_eq!(grid.downbeats().len(), 9);
    }
}
