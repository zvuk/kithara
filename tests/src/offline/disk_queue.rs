use std::{num::NonZeroUsize, path::Path};

use kithara::{
    assets::AssetStore,
    download::{Downloader, DownloaderConfig},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{CancelToken, time::Duration},
    play::{CrossfadeSettings, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl},
    queue::{Queue, QueueConfig},
};

use super::{OfflineQueue, QueueTicker, RENDER_PACE, audio_clock_pace};
use crate::{
    assets_ext::disk_asset_store,
    bufpool_ext::{TestPools, pools},
};

/// Cadence the ticker drives `QueueControl::tick` at, like the app update loop.
const TICK_INTERVAL: Duration = Duration::from_millis(50);

/// How the offline Host advances while the test waits on state.
#[derive(Clone, Copy, Debug, Default)]
pub enum RenderPacing {
    /// One block every [`RENDER_PACE`].
    #[default]
    Fixed,
    /// One block per block of audio clock.
    AudioClock,
    /// The playhead moves only where the test renders.
    Unpaced,
}

/// Product queue on an offline Host over a disk asset store, with the
/// downloader its tracks load through and a ticker standing in for the app
/// update loop.
#[non_exhaustive]
pub struct DiskQueue {
    pub queue: OfflineQueue<TestPools>,
    pub downloader: Downloader,
    pub store: AssetStore<TestPools>,
    pub ticker: QueueTicker,
}

#[bon::bon]
impl DiskQueue {
    /// Opens the queue with its cache under `cache`.
    ///
    /// # Panics
    ///
    /// Panics if the product offline Host cannot be created.
    #[builder(finish_fn = open)]
    pub async fn new(
        #[builder(start_fn)] cache: &Path,
        #[builder(default)] pacing: RenderPacing,
        crossfade_seconds: Option<f32>,
        max_concurrent_loads: Option<NonZeroUsize>,
        max_concurrent_downloads: Option<usize>,
        #[builder(default)] block_on_underrun: bool,
        #[builder(default)] net: NetOptions,
    ) -> Self {
        let store = disk_asset_store(cache);
        let pools = pools();
        let session = HostConfig::offline(pools.clone()).build();
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(session.sample_rate())
                .worker(PlayWorker::new(
                    PlayWorkerConfig::builder(pools.clone()).build(),
                ))
                .maybe_crossfade_duration(crossfade_seconds)
                .block_on_underrun(block_on_underrun)
                .build(),
        );
        let crossfade_settings = crossfade_seconds.map(|duration| CrossfadeSettings {
            duration,
            ..CrossfadeSettings::default()
        });
        let facade = Queue::new(
            QueueConfig::builder()
                .player(player)
                .store(store.clone())
                .maybe_crossfade_settings(crossfade_settings)
                .maybe_max_concurrent_loads(max_concurrent_loads)
                .build(),
        );
        let queue = match pacing {
            RenderPacing::Fixed => OfflineQueue::paced(session, facade, RENDER_PACE).await,
            RenderPacing::AudioClock => {
                let pace = audio_clock_pace(&session);
                OfflineQueue::paced(session, facade, pace).await
            }
            RenderPacing::Unpaced => OfflineQueue::new(session, facade).await,
        }
        .unwrap_or_else(|error| panic!("create product offline queue: {error}"));
        let ticker = QueueTicker::spawn(queue.control(), TICK_INTERVAL);
        let downloader = Downloader::new(
            DownloaderConfig::for_client(HttpClient::new(net, pools, CancelToken::never()))
                .maybe_max_concurrent(max_concurrent_downloads)
                .build(),
        );
        Self {
            queue,
            downloader,
            store,
            ticker,
        }
    }
}
