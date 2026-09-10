mod accelerate;
mod audio;
mod beat;
mod decode;
mod encode;
mod eq;
mod limiter;
mod resampler;
mod trim;
mod warp;

pub use accelerate::{accelerate_clear, accelerate_copy, accelerate_ramp, accelerate_wave};
pub use audio::{
    RoutePcm, blend_identity, blend_incoming, blend_join_frame, blend_multichannel, blend_outgoing,
    blend_outgoing_constant, blend_signed_frame, cursor_half, decode_negative_quarter,
    decode_quarter, route_pcm,
};
pub use beat::{
    click_silence_4s, click_silence_20s, click_silence_half, clicks_75_20s, clicks_90_20s,
    clicks_120_4s, clicks_120_20s, clicks_150_12s, clicks_150_20s, clicks_change_24s,
    clicks_change_40s,
};
#[cfg(not(target_arch = "wasm32"))]
pub use decode::{aac_init, aac_segment, flac_init};
pub use decode::{
    flac_saw, poisoned_float_wav, resampled_markers, resampled_wav_eight, resampled_wav_four,
    resampled_wav_seek,
};
pub use encode::{
    encode_partial_i16, encode_saw, encode_saw_i16, encode_scale_i16, encode_session,
    record_labels, record_signed,
};
pub use eq::{
    eq_bypass, eq_finite, eq_half, eq_impulse, eq_oscillation, eq_silence, eq_sine_40,
    eq_sine_1000, eq_sine_10000, eq_sine_15000, eq_transition,
};
pub use limiter::{
    limiter_attack, limiter_half, limiter_infinity, limiter_left, limiter_negative,
    limiter_negative_infinity, limiter_peak, limiter_quiet, limiter_recovery, limiter_right,
    limiter_silence, limiter_spike, limiter_two, limiter_unity,
};
pub use resampler::{
    apple_planar_44100, apple_planar_48000, glide_alias, glide_quadratic, glide_transition,
    glide_unity, rubato_nine, rubato_stereo,
};
pub use trim::{
    trim_codec_priming_drops_leading_frames_and_fades_in,
    trim_codec_priming_metadata_takes_precedence_when_combined, trim_ramp, trim_seek, trim_silence,
    trim_silence_trim_above_threshold_preserves_audio,
    trim_silence_trim_below_threshold_is_trimmed,
    trim_silence_trim_does_not_introduce_click_at_boundary,
    trim_silence_trim_min_frames_boundary_at_min, trim_silence_trim_min_frames_boundary_under_min,
    trim_silence_trim_no_op_with_immediate_content,
    trim_silence_trim_preserves_quiet_intro_below_threshold_then_above,
    trim_silence_trim_scan_window_exhausted_preserves_audio,
    trim_silence_trim_trailing_disabled_by_default, trim_silence_trim_trailing_enabled,
    trim_stereo_quiet, trim_stereo_silence, trim_trailing_sine,
};
pub use warp::{warp_clicks, warp_constant, warp_nominal_clicks, warp_pair, warp_sine};
