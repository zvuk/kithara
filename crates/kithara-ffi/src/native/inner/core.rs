use kithara::{
    download::{Downloader, DownloaderConfig},
    host::HostOwned,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::{Arc, Mutex},
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

#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct PeakBitrate {
    pub(crate) cellular_bps: f64,
    pub(crate) wifi_bps: f64,
}

impl PeakBitrate {
    /// Effective ABR cap, in bits/sec, derived from the configured
    /// `wifi_bps` and `cellular_bps` ceilings.
    /// Returns `None` when both are unset (`0.0`), letting ABR consider
    /// every variant. Saturates at [`u64::MAX`] for absurdly large
    /// inputs (real bitrates fit comfortably in `u64`, but `UniFFI`
    /// `f64` callers can pass anything).
    pub(crate) fn effective_cap(self) -> Option<u64> {
        const U64_MAX_AS_F64: f64 = 18_446_744_073_709_551_615.0;
        let limits = [self.wifi_bps, self.cellular_bps]
            .into_iter()
            .filter(|v| *v > 0.0);
        let cap = limits
            .reduce(f64::min)
            .filter(|v| v.is_finite() && *v > 0.0)?;
        if cap >= U64_MAX_AS_F64 {
            return Some(u64::MAX);
        }
        #[cfg_attr(
            all(),
            expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "non-negative finite bitrate clamped above; the cast is safe for any\
                          realistic peak bitrate"
            )
        )]
        let cap_u64 = cap.trunc() as u64;
        Some(cap_u64)
    }
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
    pub(super) queue_owner: HostOwned<FfiQueue>,
    pub(super) event_bridge: Mutex<Option<EventBridge>>,
    pub(super) observer: Mutex<Option<Arc<dyn PlayerObserver>>>,
    /// Bandwidth caps configured via `update_peak_bitrate`. Wifi value
    /// drives the ABR cap unless cellular is tighter; cellular is held
    /// for future network-state-aware switching.
    pub(super) peak_bitrate: Mutex<PeakBitrate>,
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
            queue_owner,
            queue,
            shutdown: cancel,
            peak_bitrate: Mutex::default(),
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
    /// Fire the master cancel pulse so the shutdown signal reaches subsystems before
    /// structural Arc teardown unwinds. The facade owns `NativeInner` by value, so this
    /// runs exactly when the `AudioPlayer` is dropped.
    fn drop(&mut self) {
        if let Err(error) = session::remove(&self.queue_owner) {
            tracing::error!(?error, "failed to remove FFI Queue from the process Host");
        }
        self.shutdown.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[kithara::test]
    fn update_peak_bitrate_remembers_both_limits() {
        let inner = NativeInner::new(FfiPlayerConfig::for_test()).expect("create player");
        inner.update_peak_bitrate(2_000_000.0, 500_000.0);
        let snapshot = *inner.peak_bitrate.lock();
        assert!((snapshot.wifi_bps - 2_000_000.0).abs() < f64::EPSILON);
        assert!((snapshot.cellular_bps - 500_000.0).abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn peak_bitrate_effective_cap_picks_min_non_zero() {
        let pb = PeakBitrate {
            wifi_bps: 2_000_000.0,
            cellular_bps: 500_000.0,
        };
        assert_eq!(pb.effective_cap(), Some(500_000));

        let pb = PeakBitrate {
            wifi_bps: 0.0,
            cellular_bps: 750_000.0,
        };
        assert_eq!(pb.effective_cap(), Some(750_000));

        let pb = PeakBitrate {
            wifi_bps: 0.0,
            cellular_bps: 0.0,
        };
        assert_eq!(pb.effective_cap(), None);
    }
}
