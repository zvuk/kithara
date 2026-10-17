use kithara_dsp::param::{DEFAULT_SETTLE_RATIO, SmootherConfig};
use kithara_effects::eq::generate_log_spaced_bands;
use kithara_play::DeckMixerConfig;

use super::*;

#[kithara::test]
fn retained_values_include_layout_and_exclude_injected_resources() {
    let rate = NonZeroU32::new(48_000).expect("fixture rate");
    let settings = crate::HostSettings::builder().sample_rate(rate).build();
    let mixer = DeckMixerConfig::builder().sample_rate(rate).build();
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .response_budget_frames(NonZeroUsize::new(448).expect("fixture response budget"))
        .build();
    let values = mixer.values();

    assert_eq!(settings.values().sample_rate.get(), 48_000);
    assert_eq!(values.sample_rate.get(), 48_000);
    assert_eq!(
        prep.response_budget_frames.map(NonZeroUsize::get),
        Some(448)
    );
    assert_eq!(prep.warp.render_quantum_frames(), None);
    assert_eq!(
        values.declick,
        SmootherConfig {
            smooth_seconds: 0.005,
            settle_ratio: DEFAULT_SETTLE_RATIO
        },
    );
    assert_eq!(values.slots.get(), 4);
    assert_eq!(generate_log_spaced_bands(values.eq_bands).len(), 10);

    let mut state = test_state(start_route_loss_stream);
    configure_sample_rate(&mut state, 48_000);
    let id = BeatGridId::allocate().expect("fixture deck id");
    assert!(matches!(
        ask(
            &mut state,
            HostCommand::Register {
                id,
                deck: Box::new(Queue::new(
                    QueueConfig::builder().prep(prep).mixer(mixer).build()
                )),
            }
        ),
        Ok(())
    ));
    assert_eq!(
        state
            .ctx
            .as_ref()
            .and_then(FirewheelContext::stream_info)
            .expect("started stream")
            .num_stream_out_channels,
        2,
    );
    assert_eq!(deck_snapshot(&mut state, id).eq.bands(), 10);
}

#[kithara::test]
fn engine_config_remains_the_live_eq_layout_owner() {
    let rate = NonZeroU32::new(48_000).expect("fixture rate");
    let mixer = DeckMixerConfig::builder()
        .sample_rate(rate)
        .slots(NonZeroUsize::new(3).expect("three slots"))
        .build();
    assert_eq!(mixer.values().sample_rate.get(), 48_000);
    assert_eq!(mixer.values().slots.get(), 3);
    assert_eq!(generate_log_spaced_bands(mixer.values().eq_bands).len(), 10);

    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .response_budget_frames(NonZeroUsize::new(448).expect("fixture response budget"))
        .build();
    let mut state = test_state(start_route_loss_stream);
    configure_sample_rate(&mut state, 48_000);
    let id = BeatGridId::allocate().expect("fixture deck id");
    assert!(matches!(
        ask(
            &mut state,
            HostCommand::Register {
                id,
                deck: Box::new(Queue::new(
                    QueueConfig::builder().prep(prep).mixer(mixer).build()
                )),
            }
        ),
        Ok(())
    ));
    fixture_command(
        &mut state,
        id,
        kithara_queue::QueueCommand::SetEqLayout(generate_log_spaced_bands(4)),
    );
    fixture_pass(&mut state, &mut 0);
    assert_eq!(deck_snapshot(&mut state, id).eq.bands(), 4);
}
