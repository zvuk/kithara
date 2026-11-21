use std::num::NonZeroU32;

use kithara_dsp::filter::FilterError;
use kithara_test_utils::{bufpool::pools, kithara};

use super::{WaveformAnalyzer, waveform::Columns};
use crate::{AnalysisParams, AnalyzerError, Bucket, MAX_BUCKETS, Waveform, consts};

const FILL: [u8; 8] = [64, 255, 0, 0, 64, 255, 0, 0];

fn analyzer() -> WaveformAnalyzer {
    WaveformAnalyzer::new(44_100, AnalysisParams::default(), &pools()).expect("the analyzer builds")
}

fn window(body: u64) -> u64 {
    if body == 0 { 0 } else { body * 3 - 1 }
}

fn set(analyzer: &mut WaveformAnalyzer, column: usize, bytes: [u8; 8]) {
    let body = u64::try_from(column / consts::COLUMNS).expect("bounded column");
    let held: &mut Columns = analyzer
        .columns
        .entry(window(body))
        .or_insert([[0; 8]; consts::COLUMNS]);
    held[column % consts::COLUMNS] = bytes;
}

fn columns(count: usize) -> WaveformAnalyzer {
    let mut analyzer = analyzer();
    for column in 0..count {
        set(&mut analyzer, column, FILL);
    }
    analyzer
}

fn extent(count: usize) -> u64 {
    64 * u64::try_from(count).expect("bounded column count")
}

fn factor(fold: u32) -> NonZeroU32 {
    NonZeroU32::new(fold).expect("positive fold factor")
}

fn bucket(wave: &Waveform, index: usize) -> Bucket {
    wave.buckets()
        .nth(index)
        .expect("bucket inside the waveform")
}

#[kithara::test]
fn factor_one_keeps_every_column() {
    let mut analyzer = columns(10_000);
    set(&mut analyzer, 1, [255, 0, 64, 0, 0, 0, 0, 128]);
    let wave = analyzer
        .snapshot(NonZeroU32::MIN, Some(extent(10_000)))
        .expect("finite columns");
    assert_eq!(wave.len(), 10_000);
    assert_eq!(bucket(&wave, 0), Bucket::from(FILL));
    assert_eq!(
        bucket(&wave, 1),
        Bucket::from([255, 0, 64, 0, 0, 0, 0, 128])
    );
    assert_eq!(wave.span_frames().get(), 64);
}

#[kithara::test]
fn factor_four_folds_each_band_and_keeps_the_remainder() {
    let mut analyzer = columns(10_002);
    set(&mut analyzer, 1, [255, 0, 64, 0, 0, 0, 0, 128]);
    set(&mut analyzer, 3, [0, 0, 0, 255, 0, 0, 255, 0]);
    set(&mut analyzer, 10_000, [255, 0, 0, 0, 0, 0, 0, 0]);
    set(&mut analyzer, 10_001, [0, 64, 0, 0, 0, 0, 0, 32]);
    let wave = analyzer
        .snapshot(factor(4), Some(extent(10_002)))
        .expect("finite columns");
    assert_eq!(wave.len(), 2_501);
    assert_eq!(
        bucket(&wave, 0),
        Bucket::from([255, 255, 64, 255, 64, 255, 255, 128])
    );
    assert_eq!(bucket(&wave, 1), Bucket::from(FILL));
    assert_eq!(
        bucket(&wave, 2_500),
        Bucket::from([255, 64, 0, 0, 0, 0, 0, 32])
    );
    assert_eq!(wave.span_frames().get(), 256);
}

#[kithara::test]
fn a_factor_states_the_same_span_on_different_track_lengths() {
    let short = columns(4_800)
        .snapshot(factor(2), Some(extent(4_800)))
        .expect("finite columns");
    let long = columns(19_200)
        .snapshot(factor(2), Some(extent(19_200)))
        .expect("finite columns");
    assert_eq!(short.span_frames().get(), 128);
    assert_eq!(short.span_frames(), long.span_frames());
    assert_eq!(10 * short.span_frames().get(), 1_280);
    assert_eq!(long.len(), 4 * short.len());
}

#[kithara::test]
fn a_ten_minute_track_fold_keeps_its_click() {
    let frames = 44_100_u64 * 60 * 10;
    let count = usize::try_from(frames.div_ceil(64)).expect("bounded column count");
    let stride = count.div_ceil(1_000);
    let click = count / 2;
    let mut analyzer = analyzer();
    set(&mut analyzer, click, [255; 8]);
    let wave = analyzer
        .snapshot(
            factor(u32::try_from(stride).expect("bounded factor")),
            Some(frames),
        )
        .expect("finite columns");
    assert_eq!(wave.len(), count.div_ceil(stride));
    assert!(wave.len() <= 1_000);
    assert_eq!(bucket(&wave, click / stride), Bucket::from([255; 8]));
    assert_eq!(
        wave.span_frames().get(),
        64 * u64::try_from(stride).expect("bounded factor")
    );
}

#[kithara::test]
#[case(3 * MAX_BUCKETS, 1, 3)]
#[case(3 * MAX_BUCKETS + 1, 1, 4)]
#[case(3 * MAX_BUCKETS, 2, 3)]
#[case(3 * MAX_BUCKETS, 4, 4)]
fn a_long_track_coarsens_to_the_smallest_factor_that_round_trips(
    #[case] count: usize,
    #[case] fold: u32,
    #[case] effective: usize,
) {
    let wave = columns(count)
        .snapshot(factor(fold), Some(extent(count)))
        .expect("finite columns");
    assert_eq!(wave.len(), count.div_ceil(effective));
    assert!(wave.len() <= MAX_BUCKETS);
    assert_eq!(
        wave.span_frames().get(),
        64 * u64::try_from(effective).expect("bounded factor")
    );
    let mut bytes = Vec::new();
    wave.write_to(&mut bytes);
    let back = Waveform::try_from(bytes.as_slice()).expect("analysis fits its codec");
    assert_eq!(back, wave);
}

#[kithara::test]
fn the_largest_factor_has_a_source_frame_span() {
    let wave = columns(10)
        .snapshot(NonZeroU32::MAX, Some(extent(10)))
        .expect("finite columns");
    assert_eq!(wave.len(), 1);
    assert_eq!(wave.span_frames().get(), u64::from(u32::MAX) * 64);
}

#[kithara::test]
fn sparse_coverage_of_a_huge_extent_stays_inside_the_format() {
    let total = 1_usize << 34;
    assert!(u64::try_from(total).expect("logical column count") > u64::from(u32::MAX));
    let last = (total / consts::COLUMNS - 1) * consts::COLUMNS;
    let mut analyzer = analyzer();
    set(&mut analyzer, last, [255; 8]);
    let wave = analyzer
        .snapshot(NonZeroU32::MIN, Some(extent(total)))
        .expect("finite columns");
    let stride = total.div_ceil(MAX_BUCKETS);
    assert_eq!(wave.len(), total.div_ceil(stride));
    assert!(wave.len() <= MAX_BUCKETS);
    assert_eq!(bucket(&wave, last / stride), Bucket::from([255; 8]));
    assert_eq!(bucket(&wave, 0), Bucket::default());
    assert_eq!(wave.span_frames().get(), extent(stride));
    assert_eq!(
        wave.buckets()
            .filter(|bucket| *bucket != Bucket::default())
            .count(),
        1
    );
}

#[kithara::test]
#[case::unknown_extent(None)]
#[case::known_extent(Some(640))]
fn unrepresentable_column_positions_are_a_shape_error(#[case] extent: Option<u64>) {
    let mut analyzer = analyzer();
    analyzer
        .columns
        .insert(u64::MAX - 1, [[0; 8]; consts::COLUMNS]);
    assert_eq!(
        analyzer.snapshot(NonZeroU32::MIN, extent).err(),
        Some(AnalyzerError::Filter(FilterError::Shape))
    );
}
