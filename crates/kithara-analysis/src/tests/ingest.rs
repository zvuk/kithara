use std::num::NonZeroU32;

use kithara_resampler::rubato::RubatoBackend;
use kithara_test_fixtures::analysis_fixtures::analysis_pcm;
use kithara_test_utils::{
    bufpool::{Pools, TestPools, pools},
    kithara,
};

use super::{
    super::analyzer::AnalyzerBuilder,
    fixtures::{self, beat_detector, chunk, sine_from, spec},
};
use crate::{
    BeatAnalysisConfig,
    analyzer::{AnalysisDemand, Extent, Ingest},
    beat::GridParams,
};

fn builder(pools: &Pools) -> AnalyzerBuilder<RubatoBackend, TestPools> {
    AnalyzerBuilder::<RubatoBackend, _>::new(pools.clone())
        .with_waveform(NonZeroU32::MIN)
        .with_beat_config(
            BeatAnalysisConfig::builder()
                .resampler_backend(RubatoBackend::default())
                .target_rate(fixtures::consts::FIXTURES_SR / 10)
                .detector_window_seconds(1)
                .detector_overlap_seconds(0)
                .build(),
        )
        .with_beat_detector(beat_detector(), GridParams::default())
}

#[kithara::test]
fn a_range_the_beat_pass_turned_down_is_told_apart_from_one_it_has(analysis_pcm: &'static [f32]) {
    let second = usize::try_from(fixtures::consts::FIXTURES_SR).unwrap_or(1);
    let step = u64::try_from(second).unwrap_or(1);
    let pools = pools();
    let mut builder = builder(&pools);
    let mut detector = builder.take_detector();
    let mut extent = Extent::default();
    let mut pass = builder
        .build(
            spec().sample_rate,
            "ingest-harness".into(),
            0,
            AnalysisDemand::ALL,
        )
        .expect("analysis buffers fit the test region");

    // Offering a range twice tells the outcomes apart, since the second offer
    // is new to nobody.
    let read =
        |at: u64, seconds: usize| chunk(&pools, sine_from(analysis_pcm, at, seconds * second), at);
    assert_eq!(
        pass.push(&read(0, 2), &mut extent, detector.as_mut())
            .expect("key estimation succeeds"),
        Ingest::Accepted
    );
    assert_eq!(
        pass.push(&read(0, 2), &mut extent, None)
            .expect("key estimation succeeds"),
        Ingest::Covered
    );

    let mut at = 2 * step;
    loop {
        assert!(at < 60 * step, "the hold is bounded");
        pass.push(&read(at, 1), &mut extent, None)
            .expect("key estimation succeeds");
        if pass
            .push(&read(at, 1), &mut extent, None)
            .expect("key estimation succeeds")
            == Ingest::Deferred
        {
            break;
        }
        at = at.saturating_add(step);
    }

    assert_eq!(
        pass.push(&read(0, 2), &mut extent, None)
            .expect("key estimation succeeds"),
        Ingest::Covered,
        "a range every consumer has stays covered"
    );
}
