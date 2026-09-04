#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    analysis::{AnalysisProducer, AnalysisWorker, AnalysisWorkerConfig, AnalyzerBuilder},
    audio::AudioObserveError,
    platform::CancelToken,
    resampler::NoResamplerBackend,
    signal::AudioSpec,
};
use kithara_integration_tests::{
    analysis_pass::stalled_reader,
    bufpool_ext::{TestPools, pools},
    kithara,
};
use kithara_test_utils::kithara::rtsan_forbid_blocking;

const RATE: u32 = 44_100;
/// Interleaved stereo samples in one offered range, about a decoder chunk.
const SAMPLES: usize = 2048;

fn spec(rate: NonZeroU32) -> AudioSpec {
    AudioSpec::new(2, rate)
}

/// The offer as the decode tick makes it, inside a region where blocking and
/// allocation are forbidden.
///
/// Under `--cfg rtsan` this aborts the process on a malloc, a free, a lock, or
/// a syscall, which is the whole assertion: the body is one downmix and one
/// copy into a transport allocated when the pass opened.
#[rtsan_forbid_blocking]
fn offer_under_rt(
    producer: &mut AnalysisProducer,
    pcm: &[f32],
    spec: AudioSpec,
    at: u64,
) -> Result<(), AudioObserveError> {
    producer.offer(pcm, spec, at)
}

/// A decoded range is offered from the decode tick, which forbids blocking.
/// The taken path is the heaviest one - the refused and closed paths return
/// before writing anything - so proving this one covers them.
#[kithara::test]
fn offering_a_decoded_range_neither_blocks_nor_allocates() {
    let rate = NonZeroU32::new(RATE).expect("test rate is non-zero");
    let cancel = CancelToken::never();
    let worker = AnalysisWorker::new(
        AnalysisWorkerConfig::for_builder(
            AnalyzerBuilder::<NoResamplerBackend, TestPools>::new(pools()).with_waveform(64),
        )
        .cancel(cancel)
        .build(),
    );
    let (_analysis, mut producer) =
        worker.analyze(stalled_reader(spec(rate)), "rt-track".into(), rate, 0);

    // Allocated before the realtime region opens, the way a decoded chunk is.
    let pcm = vec![0.25_f32; SAMPLES];
    let foreign = spec(NonZeroU32::new(48_000).expect("test rate is non-zero"));

    assert_eq!(
        offer_under_rt(&mut producer, &pcm, spec(rate), 0),
        Ok(()),
        "a range on the pass axis is taken"
    );
    assert_eq!(
        offer_under_rt(&mut producer, &pcm, foreign, 0),
        Err(AudioObserveError::UnsupportedSampleRate {
            expected: rate,
            actual: foreign.sample_rate,
        }),
        "and a range on another axis is refused, reported from the same region"
    );
}
