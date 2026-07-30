#[cfg(not(target_arch = "wasm32"))]
#[path = "../common/offline_player_harness.rs"]
mod offline_player_harness;

mod abr_auto_no_infinite_buffering;
mod cache_commit_grows;
mod commands_survive_switch_storm;
mod lane_smoke;
mod loaded_ranges_absolute;
mod offline_resume;
mod rate_applies_while_playing;
mod rate_tracks_media_time;
mod resume_from_saved_position;
mod seek_to_end_is_sane;
mod select_after_play_consumed_the_load;
mod transient_failure_no_permanent_skip;
