#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    analysis::{
        AnalysisDemand, AnalysisProducer, AnalysisWorker, AnalysisWorkerConfig, AnalyzerBuilder,
    },
    audio::AudioObserveError,
    platform::{CancelToken, time::Duration},
    resampler::NoResamplerBackend,
    signal::{AudioChunkInfo, AudioSpec},
};
use kithara_integration_tests::{
    analysis_pass::stalled_reader,
    bufpool_ext::{TestPools, pools},
    kithara,
};
use kithara_test_fixtures::play_fixtures::quarter;

const RATE: u32 = 44_100;
const FRAMES: usize = 1024;

#[kithara::rtsan_forbid_blocking]
fn offer_under_rt(
    producer: &mut AnalysisProducer,
    pcm: &[f32],
    info: &AudioChunkInfo,
) -> Result<(), AudioObserveError> {
    producer.offer(pcm, info)
}

#[kithara::test]
#[case::mono(1)]
#[case::stereo(2)]
#[case::six_channels(6)]
fn offering_a_decoded_range_neither_blocks_nor_allocates(quarter: Vec<f32>, #[case] channels: u16) {
    let rate = NonZeroU32::new(RATE).expect("test rate is non-zero");
    let spec = AudioSpec::new(channels, rate);
    let cancel = CancelToken::never();
    let worker = AnalysisWorker::new(
        AnalysisWorkerConfig::for_builder(
            AnalyzerBuilder::<NoResamplerBackend, TestPools>::new(pools()).with_waveform(64),
        )
        .cancel(cancel)
        .build(),
    );
    let (_analysis, mut producer) = worker
        .analyze(
            stalled_reader(spec),
            "rt-track".into(),
            rate,
            0,
            AnalysisDemand::ALL,
        )
        .expect("the analysis pass opens");

    // Allocated before the realtime region opens, the way a decoded chunk is.
    let pcm = &quarter[..FRAMES * usize::from(channels)];
    let foreign = AudioSpec::new(
        channels,
        NonZeroU32::new(48_000).expect("test rate is non-zero"),
    );

    let info = AudioChunkInfo {
        spec,
        frames: u32::try_from(FRAMES).expect("test frame count fits u32"),
        media_origin: Some(Duration::from_nanos(25_056_689)),
        visible_origin: Some(Duration::ZERO),
        ..Default::default()
    };
    let foreign_info = AudioChunkInfo {
        spec: foreign,
        ..info
    };

    assert_eq!(
        offer_under_rt(&mut producer, pcm, &info),
        Ok(()),
        "the heaviest path: a range on the pass axis is taken at every channel count, \
         one downmix and one copy into the transport the pass allocated when it opened"
    );
    assert_eq!(
        offer_under_rt(&mut producer, pcm, &foreign_info),
        Err(AudioObserveError::UnsupportedSampleRate {
            expected: rate,
            actual: foreign.sample_rate,
        }),
        "and a range on another axis is refused, reported from the same region \
         that would abort on a malloc, a free, a lock, or a syscall under `--cfg rtsan`"
    );
}
