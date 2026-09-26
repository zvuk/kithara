#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
mod app;
#[cfg(not(target_arch = "wasm32"))]
mod disk_queue;
pub mod host;
#[cfg(not(target_arch = "wasm32"))]
mod loader;
mod owner;
pub mod player;
#[cfg(not(target_arch = "wasm32"))]
mod ticker;
mod window;
mod worker;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
pub use app::{
    AppQueueFixture, LazyAppQueueFixture, app_disk_asset_store, app_queue, app_track_source,
    insecure_app_queue,
};
#[cfg(not(target_arch = "wasm32"))]
pub use disk_queue::{DiskQueue, RenderPacing};
pub use host::{
    MixTapProbe, OfflineHostHarness, OfflineQueue, OfflineResident, RENDER_PACE,
    assert_playhead_tracks_renderer, audio_clock_pace,
};
#[cfg(not(target_arch = "wasm32"))]
pub use loader::{LOCAL_LOAD_DEADLINE, append_loaded, append_source_loaded, asset_source};
pub use player::{
    NotificationKind, OfflinePlayer, OfflinePlayerOptions, offline_queue_fixture,
    offline_queue_fixture_with_options, resource_from_reader, resource_from_reader_with_src,
};
#[cfg(not(target_arch = "wasm32"))]
pub use ticker::QueueTicker;
pub use window::{TimedPlayerEvent, WindowStats};
pub use worker::OfflineWorker;
