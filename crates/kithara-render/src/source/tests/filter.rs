use kithara_effects::filter::{Cutoff, FilterConfig, FilterEffect, Kind};
use kithara_test_utils::bufpool::Pools;

use super::*;

fn prepared_filter(pools: &Pools, spec: AudioSpec, cutoff: Cutoff) -> FilterEffect<TestPools> {
    let config = FilterConfig::builder(pools.clone())
        .kind(Kind::LowPass)
        .cutoff_hz(cutoff)
        .build()
        .expect("admitted filter settings");
    FilterEffect::new(spec, config).expect("prepared filter")
}

#[kithara::test]
#[case::rate(2, 48_000)]
#[case::channels(1, 44_100)]
#[case::rate_and_channels(1, 48_000)]
fn filter_services_producer_format_transition_without_losing_pcm(
    #[case] channels: u16,
    #[case] rate: u32,
) {
    let pools = pools();
    let initial = AudioSpec::new(2, NonZeroU32::new(44_100).expect("initial rate"));
    let changed = AudioSpec::new(channels, NonZeroU32::new(rate).expect("changed rate"));
    let cutoff = Cutoff::checked(1_000.0).expect("admitted cutoff");
    let first = chunk(&pools, initial, 0, &[0.25; 256]);
    let second_pcm = vec![-0.125; usize::from(channels) * 128];
    let third_pcm = vec![0.0625; usize::from(channels) * 128];
    let second = chunk(&pools, changed, 128, &second_pcm);
    let third = chunk(&pools, changed, 256, &third_pcm);
    let mut reference = prepared_filter(&pools, changed, cutoff);
    let expected_second = reference
        .process(chunk(&pools, changed, 128, &second_pcm))
        .expect("new-format reference")
        .samples
        .to_vec();
    let expected_third = reference
        .process(chunk(&pools, changed, 256, &third_pcm))
        .expect("continued reference history")
        .samples
        .to_vec();
    let discontinuity = Arc::new(Mutex::new(SourceDiscontinuity::new(7, initial)));
    let raw = RevisionSource {
        chunks: VecDeque::from([first, second, third]),
        discontinuity: Arc::clone(&discontinuity),
    };
    let effects: Vec<Box<dyn AudioEffect>> =
        vec![Box::new(prepared_filter(&pools, initial, cutoff))];
    let mut source = source_stage(&pools, raw, effects, initial);
    let TrackStep::Produced(Fetch::Data { data, .. }) = source.step_track() else {
        panic!("initial filtered PCM must be produced");
    };
    assert_eq!(data.frames(), 128);
    assert_eq!(data.spec(), initial);
    assert!(data.samples.iter().any(|sample| *sample != 0.0));

    *discontinuity.lock() = SourceDiscontinuity::new(8, changed);
    for expected in [expected_second, expected_third] {
        flush_deferred(&mut source);
        let TrackStep::Produced(Fetch::Data { data, .. }) = source.step_track() else {
            panic!("serviced producer format must emit every accepted PCM frame");
        };
        assert_eq!(data.spec(), changed);
        assert_eq!(data.frames(), 128);
        assert_eq!(data.samples.len(), usize::from(channels) * 128);
        assert_eq!(&data.samples[..], &expected);
    }
}

#[kithara::test]
fn filter_refuses_invalid_new_nyquist_as_terminal_producer_failure() {
    let pools = pools();
    let initial = AudioSpec::new(2, NonZeroU32::new(48_000).expect("initial rate"));
    let changed = AudioSpec::new(2, NonZeroU32::new(32_000).expect("changed rate"));
    let discontinuity = Arc::new(Mutex::new(SourceDiscontinuity::new(7, initial)));
    let raw = RevisionSource {
        chunks: VecDeque::from([
            chunk(&pools, initial, 0, &[0.25; 256]),
            chunk(&pools, changed, 128, &[0.25; 256]),
        ]),
        discontinuity: Arc::clone(&discontinuity),
    };
    let effects: Vec<Box<dyn AudioEffect>> =
        vec![Box::new(prepared_filter(&pools, initial, Cutoff::MAX))];
    let mut source = source_stage(&pools, raw, effects, initial);
    assert!(matches!(source.step_track(), TrackStep::Produced(_)));
    *discontinuity.lock() = SourceDiscontinuity::new(8, changed);
    flush_deferred(&mut source);
    for _ in 0..2 {
        assert!(matches!(
            source.step_track(),
            TrackStep::Failed(TrackFailureKind::Render)
        ));
    }
}
