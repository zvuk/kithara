use std::num::NonZeroU32;

use kithara_test_utils::{bufpool::pools, kithara};

use super::WaveformAnalyzer;
use crate::{AnalysisParams, Bucket, MAX_BUCKETS, Waveform};

fn windows(count: u64) -> WaveformAnalyzer {
    let pools = pools();
    let mut analyzer = WaveformAnalyzer::new(
        44_100,
        AnalysisParams::builder().band_gain([1.0; 3]).build(),
        &pools,
    )
    .expect("the analyzer builds");
    for index in 0..count {
        analyzer.bands.insert(index, [0.25, 1.0, 0.0]);
    }
    analyzer
}

#[kithara::test]
fn factor_one_keeps_every_window() {
    let mut analyzer = windows(10_000);
    analyzer.bands.insert(1, [1.0, 0.0, 0.25]);
    let wave = analyzer.snapshot(NonZeroU32::MIN, None);
    assert_eq!(wave.len(), 10_000);
    assert_eq!(wave.buckets()[0], Bucket::new(0.5, 1.0, 0.0));
    assert_eq!(wave.buckets()[1], Bucket::new(1.0, 0.0, 0.5));
}

#[kithara::test]
fn factor_four_folds_each_band_and_keeps_the_remainder() {
    let mut analyzer = windows(10_002);
    analyzer.bands.insert(1, [1.0, 0.0, 0.25]);
    analyzer.bands.insert(3, [0.0, 0.0, 1.0]);
    analyzer.bands.insert(10_000, [1.0, 0.0, 0.0]);
    analyzer.bands.insert(10_001, [0.0, 0.25, 0.0]);
    let wave = analyzer.snapshot(NonZeroU32::new(4).unwrap(), None);
    assert_eq!(wave.len(), 2_501);
    assert_eq!(wave.buckets()[0], Bucket::new(1.0, 1.0, 1.0));
    assert_eq!(wave.buckets()[1], Bucket::new(0.5, 1.0, 0.0));
    assert_eq!(wave.buckets()[2_500], Bucket::new(1.0, 0.5, 0.0));
    assert_eq!(wave.span_frames().get(), 4 * analyzer.hop());
}

#[kithara::test]
fn a_factor_states_the_same_span_on_different_track_lengths() {
    let factor = NonZeroU32::new(2).unwrap();
    let short = windows(100).snapshot(factor, None);
    let long = windows(400).snapshot(factor, None);
    assert_eq!(short.span_frames().get(), 2048);
    assert_eq!(short.span_frames(), long.span_frames());
    assert_eq!(10 * short.span_frames().get(), 20_480);
    assert_eq!(long.len(), 4 * short.len());
}

#[kithara::test]
#[case(3 * MAX_BUCKETS, 1, 3)]
#[case(3 * MAX_BUCKETS + 1, 1, 4)]
#[case(3 * MAX_BUCKETS, 2, 3)]
#[case(3 * MAX_BUCKETS, 4, 4)]
fn a_long_track_coarsens_to_the_smallest_factor_that_round_trips(
    #[case] count: usize,
    #[case] factor: u32,
    #[case] effective: u64,
) {
    let mut analyzer = windows(u64::try_from(count).unwrap());
    let wave = analyzer.snapshot(NonZeroU32::new(factor).unwrap(), None);
    assert_eq!(
        wave.len(),
        count.div_ceil(usize::try_from(effective).unwrap())
    );
    assert!(wave.len() <= MAX_BUCKETS);
    assert_eq!(wave.span_frames().get(), effective * analyzer.hop());
    let mut bytes = Vec::new();
    wave.write_to(&mut bytes);
    let back = Waveform::try_from(bytes.as_slice()).expect("analysis fits its codec");
    assert_eq!(back.buckets(), wave.buckets());
    assert_eq!(back.span_frames(), wave.span_frames());
}

#[kithara::test]
fn the_largest_factor_has_a_source_frame_span() {
    let mut analyzer = windows(10);
    let wave = analyzer.snapshot(NonZeroU32::MAX, None);
    assert_eq!(wave.len(), 1);
    assert_eq!(
        wave.span_frames().get(),
        u64::from(u32::MAX) * analyzer.hop()
    );
}
