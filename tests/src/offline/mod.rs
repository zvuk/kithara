mod app;
pub mod harness;
pub mod host;
pub mod player;
mod window;

pub use app::{AppQueueFixture, LazyAppQueueFixture, insecure_app_queue};
pub use harness::{OfflinePlayerHarness, OfflinePlayerOptions, offline_queue_fixture};
pub use host::{
    MixTapProbe, OfflineHostHarness, OfflineQueue, OfflineResident, drive_queue_ticks,
    offline_gain_window,
};
pub use player::{
    NotificationKind, OfflinePlayer, resource_from_reader, resource_from_reader_with_src,
};
pub use window::{
    TimedPlayerEvent, WindowStats, deinterleave_left, max_silence_run, mean_abs, peak, rms,
};
