use std::num::NonZeroU32;

use kithara_events::EventBus;
use kithara_host::{Host, HostConfig, HostSettings};
use kithara_play::{PlayWorker, PlayWorkerConfig, ResourcePrep, SessionEvent};
use kithara_queue::{Queue, QueueConfig};
use kithara_test_utils::{
    bufpool::{TestPools, pools},
    kithara,
};

fn make_engine() -> Host<TestPools> {
    Host::new(
        HostConfig::offline(pools())
            .settings(
                HostSettings::builder()
                    .sample_rate(NonZeroU32::new(44_100).expect("fixture sample rate"))
                    .build(),
            )
            .build(),
    )
    .expect("fixture host")
}

#[kithara::test]
fn engine_config_defaults() {
    let engine = make_engine();
    assert_eq!(engine.output_sample_rate().output(), 44100);
}

#[kithara::test]
fn engine_config_builder() {
    let config = HostConfig::offline(pools())
        .settings(
            HostSettings::builder()
                .sample_rate(NonZeroU32::new(48_000).expect("fixture sample rate is non-zero"))
                .build(),
        )
        .build();
    let engine = Host::new(config).expect("fixture host");
    assert_eq!(engine.output_sample_rate().output(), 48000);
}

#[kithara::test]
fn an_engine_holds_no_slot_until_its_host_seats_it() {
    assert!(make_engine().is_empty());
}

#[kithara::test]
fn engine_subscribe_works() {
    let mut engine = make_engine();
    let bus = EventBus::default();
    let _rx = bus.subscribe::<SessionEvent>();
    let queue = Queue::new(
        QueueConfig::builder()
            .prep(
                ResourcePrep::builder()
                    .bus(bus)
                    .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
                    .build(),
            )
            .build(),
    );
    let _owned = engine
        .insert(queue)
        .expect("host registers the session event bus");
}

#[kithara::test]
fn engine_master_sample_rate_returns_config_until_a_host_takes_it() {
    let config = HostConfig::offline(pools())
        .settings(
            HostSettings::builder()
                .sample_rate(NonZeroU32::new(48_000).expect("fixture sample rate is non-zero"))
                .build(),
        )
        .build();
    let engine = Host::new(config).expect("fixture host");
    assert_eq!(engine.output_sample_rate().output(), 48000);
}
