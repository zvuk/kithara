use crate::session;

/// Current playback position in seconds (read from shared atomics).
pub fn bridge_position_secs() -> f64 {
    session::bridge_position_secs()
}

/// Current media duration in seconds (read from shared atomics).
pub fn bridge_duration_secs() -> f64 {
    session::bridge_duration_secs()
}

/// Whether playback is active (read from shared atomics).
pub fn bridge_is_playing() -> bool {
    session::bridge_is_playing()
}
