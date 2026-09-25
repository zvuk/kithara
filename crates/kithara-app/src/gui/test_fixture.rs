use iced::window::Id;
use kithara::{
    assets::StorageBackend,
    download::{Downloader, DownloaderConfig},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{CancelToken, sync::Arc},
    play::{PlayWorkerConfig, policy::DomainKeyPolicy},
};

use super::{
    app::{Decks, Kithara},
    ui::{AppUi, package::Package},
};
use crate::{
    broadcast::Broadcaster,
    catalog::Catalog,
    config::{AppBroadcastConfig, AppConfig, AppDrm},
    deck::{Deck, DeckId, DeckSet},
    pools::{self, AppHost, AppStore, AppWorker},
    state::test_fixture::controller,
};

pub(super) fn state() -> Kithara {
    let config = config();
    let mut host = AppHost::new(HostConfig::offline(config.worker.pools().clone()).build())
        .expect("test host");
    let decks: Vec<Deck> = (0..2)
        .map(|index| {
            Deck::build(DeckId(index), &config, &mut host).expect("host accepts the test deck")
        })
        .collect();
    let controllers = decks
        .iter()
        .map(|deck| {
            (
                deck.id,
                controller(
                    deck.queue.control().clone(),
                    Arc::clone(&deck.timestretch),
                    deck.cancel_child(),
                ),
            )
        })
        .collect();
    let session = DeckSet::new(host, decks);
    let decks = Decks::new(controllers).expect("fixture has decks");
    let catalog = Catalog::new(vec![
        "/music/local.flac".to_string(),
        "https://example.test/stream.m3u8".to_string(),
    ]);
    let ui = AppUi::new(Package::load(None).expect("shipped UI package"), &config.ui)
        .expect("shipped UI compiles");
    Kithara::mounted(
        session,
        decks,
        catalog,
        config,
        ui,
        Broadcaster::new(AppBroadcastConfig::default()),
        Id::unique(),
    )
}

fn config() -> AppConfig {
    let shutdown = CancelToken::root();
    let pools = pools::build(&pools::PoolsSection::default()).expect("valid app pool policy");
    let worker = AppWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::builder().build(),
            pools.clone(),
            shutdown.child(),
        ))
        .build(),
    );
    let store = AppStore::builder(pools)
        .backend(StorageBackend::Memory)
        .build();
    AppConfig::builder()
        .drm(AppDrm::new(DomainKeyPolicy::new(Vec::new())))
        .downloader(downloader)
        .shutdown(shutdown)
        .worker(worker)
        .store(store)
        .build()
}

#[kithara::test(native, flash(false))]
fn app_config_snapshot_keeps_owned_values_and_nested_analysis_policy() {
    let mut config = config();
    config.waveform_max_buckets = 256;
    config.beat_analysis.target_rate = 48_000;

    let values = kithara_config::Config::values(&config);

    assert_eq!(values.waveform_max_buckets, 256);
    assert_eq!(values.beat_analysis.target_rate, 48_000);
    assert_eq!(values.ui.max_arena_bytes, config.ui.max_arena_bytes);
}
