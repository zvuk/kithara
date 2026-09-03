use kithara_resampler::rubato::RubatoBackend;
use kithara_test_utils::kithara;

use super::{
    super::analyzer::AnalyzerBuilder,
    fixtures::{SR, beat_detector, chunk, sine_from, spec},
};
use crate::{
    BeatAnalysisConfig,
    analyzer::Ingest,
    beat::GridParams,
    test_pools::{Pools, TestPools, pools},
};

/// One-second windows at a tenth of the source rate: eight seconds of source
/// fill the hold, so what follows is turned down rather than taken.
fn builder(pools: &Pools) -> AnalyzerBuilder<RubatoBackend, TestPools> {
    AnalyzerBuilder::<RubatoBackend, _>::new(pools.clone())
        .with_waveform(64)
        .with_beat_config(
            BeatAnalysisConfig::builder()
                .resampler_backend(RubatoBackend::default())
                .target_rate(SR / 10)
                .detector_window_seconds(1)
                .detector_overlap_seconds(0)
                .build(),
        )
        .with_beat_detector(beat_detector(), GridParams::default())
}

#[kithara::test]
fn a_range_the_beat_pass_turned_down_is_told_apart_from_one_it_has() {
    let second = usize::try_from(SR).unwrap_or(1);
    let step = u64::try_from(second).unwrap_or(1);
    let pools = pools();
    let mut builder = builder(&pools);
    let mut detector = builder.take_detector();
    let mut pass = builder
        .build(spec().sample_rate, "ingest-harness".into())
        .expect("analysis buffers fit the test region");

    // The first second is read; the rest arrives with the detector left
    // behind, so the hold fills. Offering a range twice tells the outcomes
    // apart, since the second offer is new to nobody.
    let read = |at: u64, seconds: usize| chunk(&pools, &sine_from(at, seconds * second), at);
    assert_eq!(pass.push(&read(0, 2), detector.as_mut()), Ingest::Accepted);
    assert_eq!(pass.push(&read(0, 2), None), Ingest::Covered);

    let mut at = 2 * step;
    loop {
        assert!(at < 60 * step, "the hold is bounded");
        pass.push(&read(at, 1), None);
        if pass.push(&read(at, 1), None) == Ingest::Deferred {
            break;
        }
        at = at.saturating_add(step);
    }

    assert_eq!(
        pass.push(&read(0, 2), None),
        Ingest::Covered,
        "a range every consumer has stays covered"
    );
}
