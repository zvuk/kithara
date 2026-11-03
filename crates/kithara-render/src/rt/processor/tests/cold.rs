use firewheel::{
    StreamInfo,
    log::{RealtimeLoggerConfig, realtime_logger},
    node::{AudioNodeProcessor, ProcStore, ProcStreamCtx},
};
use kithara_dsp::filter::FilterError;
use kithara_effects::{EffectPrepareError, eq::generate_log_spaced_bands};

use super::*;

fn stream(rate: u32, block: u32) -> StreamInfo {
    let sample_rate = NonZeroU32::new(rate).expect("positive rate");
    StreamInfo {
        sample_rate,
        sample_rate_recip: f64::from(rate).recip(),
        prev_sample_rate: NonZeroU32::new(RATE).expect("old rate"),
        max_block_frames: NonZeroU32::new(block).expect("positive block"),
        num_stream_in_channels: 0,
        num_stream_out_channels: 2,
        input_to_output_latency_seconds: 0.0,
        declick_frames: NonZeroU32::new(128).expect("declick"),
    }
}

#[kithara::test]
#[case::own_filter_refusal(12_000, 512, false)]
#[case::later_owner_refusal(48_000, 512, true)]
#[case::shape_refusal(44_100, 1_024, false)]
fn refused_attempt_preserves_state_across_matching_successful_lifecycle(
    #[case] attempted_rate: u32,
    #[case] attempted_block: u32,
    #[case] later_owner_refuses: bool,
) {
    let (mut actual, _actual_ends) = mixer();
    let (mut reference, _reference_ends) = mixer();
    let config = EqConfig::builder(pools()).build();
    let layout = |count| Box::new(EqLayout::new(&config, &generate_log_spaced_bands(count),
        NonZeroU32::new(RATE).expect("initial rate")).expect("valid layout"));
    for mixer in [&mut actual, &mut reference] {
        mixer.mixer.deck.render.set_eq_gain(0, GainDb::from(-6.0));
        mixer.mixer.deck.render.finish(&mut [0.25; 17], &mut [-0.125; 17]);
        assert!(mixer.mixer.deck.render.take_eq_layout(layout(3)).expect("incoming").is_none());
        mixer.mixer.deck.render.finish(&mut [0.25], &mut [-0.125]);
        assert!(mixer.mixer.deck.render.take_eq_layout(layout(10)).expect("pending").is_none());
        mixer.mixer.deck.render.set_eq_gain(2, GainDb::from(3.0));
    }
    let mut store = ProcStore::with_capacity(1);
    let (mut logger, _messages) = realtime_logger(RealtimeLoggerConfig::default());
    let mut context = ProcStreamCtx { store: &mut store, logger: &mut logger };
    actual.mixer.stream_stopped(&mut context);
    reference.mixer.stream_stopped(&mut context);
    let result = actual.mixer.prepare_new_stream(&stream(attempted_rate, attempted_block));
    if later_owner_refuses {
        result.expect("this owner prepares, but the transaction never commits");
    } else {
        let error = result.expect_err("controlled refusal");
        let error = error.0.downcast_ref::<EffectPrepareError>().expect("typed payload");
        if attempted_block > u32::try_from(BLOCK).expect("fixture block") {
            assert!(matches!(error, EffectPrepareError::Shape));
        } else {
            assert!(matches!(error, EffectPrepareError::Filter(FilterError::Parameters)));
        }
    }
    assert_eq!(actual.mixer.deck.sample_rate, reference.mixer.deck.sample_rate);
    assert_eq!(actual.mixer.deck.declick_frames, reference.mixer.deck.declick_frames);
    assert_eq!(actual.mixer.deck.mix, reference.mixer.deck.mix);
    let mut actual_gains = [GainDb::DEFAULT; 10];
    let mut reference_gains = actual_gains;
    assert_eq!(actual.mixer.deck.render.read_eq(&mut actual_gains), reference.mixer.deck.render.read_eq(&mut reference_gains));
    assert_eq!(actual_gains, reference_gains);
    let old = stream(RATE, u32::try_from(BLOCK).expect("fixture block"));
    for mixer in [&mut actual, &mut reference] {
        mixer.mixer.prepare_new_stream(&old).expect("matching successful old-format start");
        mixer.mixer.new_stream(&old, &mut context);
    }
    assert_eq!(actual.mixer.deck.sample_rate.get(), RATE);
    for _ in 0..8 {
        let (mut left, mut right) = ([0.25; 257], [-0.125; 257]);
        let (mut expected_left, mut expected_right) = (left, right);
        actual.mixer.deck.render.finish(&mut left, &mut right);
        reference.mixer.deck.render.finish(&mut expected_left, &mut expected_right);
        assert_eq!(left.map(f32::to_bits), expected_left.map(f32::to_bits));
        assert_eq!(right.map(f32::to_bits), expected_right.map(f32::to_bits));
    }
}
