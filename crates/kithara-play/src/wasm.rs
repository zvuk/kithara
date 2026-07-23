mod playback;
mod session;

use kithara_platform::thread::assert_main_thread;
pub use playback::{bridge_duration_secs, bridge_is_playing, bridge_position_secs};
pub use session::{
    ensure_main_session, remote_session, tick_and_poll, warm_up_audio, worker_session_channel,
};

/// Start the main-thread WebCodecs capability probe.
pub fn spawn_webcodecs_probe() {
    assert_main_thread("spawn_webcodecs_probe");
    kithara_decode::spawn_webcodecs_probe();
}
