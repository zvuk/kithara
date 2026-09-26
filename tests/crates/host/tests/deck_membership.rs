#![cfg(not(target_arch = "wasm32"))]

use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    host::HostConfig,
    play::{PlayError, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, SessionError},
    sync::SyncGroup,
    warp::WarpConfig,
};
use kithara_integration_tests::{offline::OfflineHostHarness, smoothing::Consts};
use kithara_test_utils::bufpool::pools;

#[kithara::test(tokio)]
async fn failed_deck_preparation_releases_host_membership() {
    let region = pools();
    let sample_rate = NonZeroU32::new(Consts::SAMPLE_RATE).expect("sample rate");
    let config = HostConfig::offline(region.clone())
        .sample_rate(sample_rate)
        .max_block_frames(NonZeroU32::new(Consts::BLOCK_FRAMES as u32).expect("block size"))
        .build();
    let host = OfflineHostHarness::new(config).await.expect("offline host");
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let invalid = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(sample_rate)
            .worker(worker.clone())
            .warp(
                WarpConfig::builder()
                    .render_quantum_frames(NonZeroUsize::new(32).expect("quantum"))
                    .build(),
            )
            .response_budget_frames(NonZeroUsize::new(1).expect("budget"))
            .build(),
    );
    assert!(matches!(
        host.insert(invalid).await,
        Err(PlayError::Session(
            SessionError::ResponseBudgetExceeded { .. }
        ))
    ));
    host.with(|host| {
        assert!(host.topology().expect("host topology").members().is_empty());
        assert!(
            host.sample_rate()
                .expect("host sample rate")
                .measured
                .is_none(),
            "failed preparation must close an otherwise idle stream"
        );
    })
    .await;
    let valid = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(sample_rate)
            .worker(worker)
            .build(),
    );
    let deck = host
        .insert(valid)
        .await
        .expect("host can prepare the next deck");
    host.with(move |host| {
        assert!(
            host.sample_rate().expect("sample rate").measured.is_none(),
            "inserting an idle deck must not start the output stream"
        );
        deck.set_eq_gain(0, -6.0).expect("configure idle EQ");
        assert_eq!(deck.eq_gain(0), Some(-6.0));
        deck.play();
        assert!(host.sample_rate().expect("sample rate").measured.is_some());
        assert_eq!(deck.eq_gain(0), Some(-6.0));
        deck.pause();
    })
    .await;
    host.close().await;
}
