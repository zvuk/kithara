#[cfg(not(target_arch = "wasm32"))]
mod app;
#[cfg(not(target_arch = "wasm32"))]
pub mod harness;
pub mod host;
pub mod player;
mod window;
mod worker;

#[cfg(not(target_arch = "wasm32"))]
pub use app::{AppQueueFixture, LazyAppQueueFixture, app_queue, insecure_app_queue};
#[cfg(not(target_arch = "wasm32"))]
pub use harness::{OfflinePlayerHarness, OfflinePlayerOptions, offline_queue_fixture};
#[cfg(not(target_arch = "wasm32"))]
pub use host::QueueTicker;
pub use host::{
    MixTapProbe, OfflineHostHarness, OfflineQueue, OfflineResident, offline_gain_window,
};
pub use player::{
    NotificationKind, OfflinePlayer, resource_from_reader, resource_from_reader_with_src,
};
pub use window::{
    TimedPlayerEvent, WindowStats, deinterleave_left, max_silence_run, mean_abs, peak, rms,
};
pub use worker::OfflineWorker;
