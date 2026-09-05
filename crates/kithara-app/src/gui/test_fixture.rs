use iced::window::Id;
use kithara::{
    assets::StorageBackend,
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{CancelToken, sync::Arc},
    play::PlayWorkerConfig,
    stream::dl::{Downloader, DownloaderConfig},
};

use super::{
    app::{Decks, Kithara},
    ui::{AppUi, package::Package},
};
use crate::{
    broadcast::Broadcaster,
    catalog::Catalog,
    config::{AppBroadcastConfig, AppConfig},
    deck::{Deck, DeckId, DeckSet},
    pools::{self, AppHost, AppStore, AppWorker},
    state::test_fixture::controller,
};

pub(super) fn state() -> Kithara {
    let config = config();
    let mut host = AppHost::new(HostConfig::builder().build()).expect("test host");
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
    let ui =
        AppUi::new(Package::load(None).expect("shipped UI package")).expect("shipped UI compiles");
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
    let pools = pools::build().expect("valid app pool policy");
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
        .downloader(downloader)
        .shutdown(shutdown)
        .worker(worker)
        .store(store)
        .build()
}
