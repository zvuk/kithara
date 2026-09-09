#[cfg(not(target_arch = "wasm32"))]
mod app;
#[cfg(not(target_arch = "wasm32"))]
pub mod harness;
pub mod host;
mod owner;
pub mod player;
#[cfg(not(target_arch = "wasm32"))]
mod ticker;
mod window;
mod worker;

#[cfg(not(target_arch = "wasm32"))]
pub use app::{AppQueueFixture, LazyAppQueueFixture, app_queue, insecure_app_queue};
#[cfg(not(target_arch = "wasm32"))]
pub use harness::{OfflinePlayerHarness, OfflinePlayerOptions, offline_queue_fixture};
pub use host::{
    MixTapProbe, OfflineHostHarness, OfflineQueue, OfflineResident, offline_gain_window,
};
pub use player::{
    NotificationKind, OfflinePlayer, resource_from_reader, resource_from_reader_with_src,
};
#[cfg(not(target_arch = "wasm32"))]
pub use ticker::QueueTicker;
pub use window::{
    TimedPlayerEvent, WindowStats, deinterleave_left, max_silence_run, mean_abs, peak, rms,
};
pub use worker::OfflineWorker;
