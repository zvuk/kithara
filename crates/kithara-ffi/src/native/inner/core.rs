use kithara::{
    download::{Downloader, DownloaderConfig},
    host::HostOwned,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::{Arc, Mutex},
        tokio::{runtime::Handle, task::spawn_blocking_on},
    },
    play::{DeckMixerConfig, PlayWorkerConfig, ResourcePrep, TrackSettings},
    queue::{QueueConfig, QueueSettings},
    warp::{StretchKind, WarpCapabilities, WarpConfig},
};

use super::resource::ResourceFactory;
use crate::{
    EventBridge, Router,
    config::FfiPlayerConfig,
    native::session,
    observer::{FfiKeyProcessor, PlayerObserver},
    pools::{FfiQueue, FfiQueueControl, FfiWorker},
    registry::ItemRegistry,
    types::{FfiError, FfiKeyRule},
};

/// The Warp configuration every FFI player starts its tracks on: keylocked on
/// Apple when the selected backend preserves pitch.
fn player_warp() -> WarpConfig {
    let keylock = cfg!(all(feature = "apple", target_vendor = "apple"))
        && StretchKind::default()
            .capabilities()
            .contains(WarpCapabilities::KEYLOCK);
    WarpConfig::builder().keylock(keylock).build()
}

/// Build the default `NetOptions`. The `dev` feature enables the
/// `insecure` flag for local test servers; release builds always
/// validate TLS.
fn default_net_options() -> NetOptions {
    const INSECURE: bool = cfg!(feature = "dev");
    NetOptions::builder().is_insecure(INSECURE).build()
}

/// Native engine behind the [`crate::player::AudioPlayer`] facade.
pub(crate) type Inner = NativeInner;

/// Native (Apple / Android) implementation of the FFI player. Owns the
/// queue lifecycle, item registry, event bridge, and resource factory. The
/// `AudioPlayer` facade delegates every public method here so the
/// `UniFFI` surface stays a thin shell over this engine.
pub(crate) struct NativeInner {
    /// Swift-owned items indexed by `TrackId`. Populated by `insert`,
    /// drained by `remove` / `remove_all_items`. Lets `items` return
    /// the same `AudioPlayerItem` instances that Swift handed in (preserves
    /// identity + active per-item observer wiring).
    pub(super) items: Arc<Mutex<ItemRegistry>>,
    pub(super) resources: ResourceFactory,
    /// Cancellation root for player-owned work; the shared store owns a
    /// separate scope.
    pub(super) shutdown: CancelToken,
    pub(super) queue: FfiQueueControl,
    /// The queue's seat in the process Host; taken only by `Drop`.
    pub(super) queue_owner: Option<HostOwned<FfiQueue>>,
    pub(super) event_bridge: Mutex<Option<EventBridge>>,
    pub(super) observer: Mutex<Option<Arc<dyn PlayerObserver>>>,
}

impl NativeInner {
    pub(crate) fn new(config: FfiPlayerConfig) -> Result<Self, FfiError> {
        let FfiPlayerConfig {
            key_options,
            store,
            eq_band_count,
            auth_token,
            playing_rate,
            playback_order,
            action_at_item_end,
            crossfade_settings,
        } = config;
        let cancel = CancelToken::root();
        let pools = store.pools().clone();
        let worker = FfiWorker::new(
            PlayWorkerConfig::builder(pools.clone())
                .cancel(cancel.child())
                .runtime(Some(crate::FFI_RUNTIME.clone()))
                .build(),
        );
        let queue_store = store.handle().clone();
        let warp = player_warp();
        let track = TrackSettings::builder().keylock(warp.keylock()).build();
        let prep = ResourcePrep::builder()
            .worker(worker)
            .warp(warp)
            .cancel(cancel.child())
            .build();
        let queue_config = QueueConfig::builder()
            .mixer(
                DeckMixerConfig::builder()
                    .eq_bands(eq_band_count as usize)
                    .build(),
            )
            .prep(prep)
            .track(track)
            .cancel(cancel.child())
            .runtime(crate::FFI_RUNTIME.clone())
            .store(queue_store)
            .playback_order(playback_order.try_into()?)
            .action_at_item_end(action_at_item_end.try_into()?)
            .settings(
                QueueSettings::builder()
                    .crossfade(crossfade_settings.try_into()?)
                    .build(),
            )
            .build();
        let queue_owner = session::insert(FfiQueue::new(queue_config))?;
        let queue = queue_owner.control().clone();
        let net = default_net_options();
        let downloader = Downloader::new(
            DownloaderConfig::for_client(HttpClient::new(net, pools, cancel.child()))
                .runtime(crate::FFI_RUNTIME.clone())
                .build(),
        );
        let resources = ResourceFactory::new(store, downloader, key_options);
        let inner = Self {
            resources,
            queue_owner: Some(queue_owner),
            queue,
            shutdown: cancel,
            observer: Mutex::default(),
            event_bridge: Mutex::default(),
            items: Arc::new(Mutex::default()),
        };
        inner.setup_network(auth_token);
        inner.set_playing_rate(playing_rate)?;
        Ok(inner)
    }

    pub(crate) fn set_observer(&self, observer: Arc<dyn PlayerObserver>) {
        let rx = self.queue.subscribe();

        let bridge = EventBridge::spawn(
            rx,
            Router::new(Arc::clone(&observer), Arc::clone(&self.items)),
            self.queue.clone(),
            CancelToken::never(),
        );

        let mut eb = self.event_bridge.lock();
        let mut obs = self.observer.lock();
        *eb = Some(bridge);
        *obs = Some(observer);
        drop(obs);
        drop(eb);
    }

    delegate::delegate! {
        to self.resources {
            pub(crate) fn setup_hls_aes(&self, processor: Arc<dyn FfiKeyProcessor>);
            pub(crate) fn setup_hls_aes_with_rule(&self, rule: FfiKeyRule);
            pub(crate) fn setup_network(&self, auth_token: String);
        }
    }
}

impl Drop for NativeInner {
    /// Leave the process Host, stop the event bridge, then fire the master
    /// cancel pulse. The facade owns `NativeInner` by value, so this runs
    /// exactly when the `AudioPlayer` is dropped. A drop inside a runtime
    /// context (an observer releasing the final player from its callback)
    /// hands that blocking teardown to the runtime's blocking pool instead of
    /// stalling the async task that delivered the event.
    fn drop(&mut self) {
        let owner = self.queue_owner.take();
        let bridge = self.event_bridge.lock().take();
        let shutdown = self.shutdown.clone();
        let teardown = move || {
            if let Some(owner) = owner
                && let Err(error) = session::remove(&owner)
            {
                tracing::error!(?error, "failed to remove FFI Queue from the process Host");
            }
            drop(bridge);
            shutdown.cancel();
        };
        match Handle::try_current() {
            Ok(runtime) => drop(spawn_blocking_on(&runtime, teardown)),
            Err(_) => teardown(),
        }
    }
}
