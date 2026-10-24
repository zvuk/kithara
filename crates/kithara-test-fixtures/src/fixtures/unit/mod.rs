mod accelerate;
mod audio;
mod beat;
mod decode;
mod encode;
mod eq;
mod fmp4;
mod limiter;
mod resampler;
mod trim;
mod warp;

pub use accelerate::{accelerate_ramp, accelerate_wave};
#[cfg(feature = "encoded")]
pub use audio::decoder_opus_packet;
pub use audio::{
    RoutePcm, blend_identity, blend_incoming, blend_join_frame, blend_multichannel, blend_outgoing,
    blend_outgoing_constant, blend_signed_frame, broadcast_tone_wav, cochlea_control_wav,
    cursor_half, decode_negative_quarter, decode_quarter, host_manual_incoming,
    host_manual_outgoing, host_metronome_held, host_metronome_tone, metronome_sine_wav,
    render_chirp, render_crossfade_incoming, render_crossfade_outgoing, render_readiness_pcm,
    render_seek_after, render_seek_before, render_sine, route_pcm, stretch_kick_body,
    warp_native_tone,
};
pub use beat::{
    click_silence_4s, click_silence_20s, click_silence_half, clicks_75_20s, clicks_90_20s,
    clicks_120_4s, clicks_120_20s, clicks_150_12s, clicks_150_20s, clicks_change_24s,
    clicks_change_40s,
};
#[cfg(feature = "encoded")]
pub use decode::flac_saw;
pub use decode::{
    poisoned_float_wav, resampled_markers, resampled_wav_eight, resampled_wav_four,
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
#[cfg(all(feature = "hls", not(target_arch = "wasm32")))]
pub use fmp4::{aac_init, aac_segment, flac_init};
pub use limiter::{
    limiter_attack, limiter_half, limiter_infinity, limiter_intersample, limiter_left,
    limiter_negative, limiter_negative_infinity, limiter_peak, limiter_quiet, limiter_recovery,
    limiter_right, limiter_silence, limiter_sine, limiter_smooth, limiter_spike, limiter_two,
};
pub use resampler::{
    apple_planar_44100, apple_planar_48000, glide_alias, glide_quadratic, glide_transition,
    glide_unity, rubato_nine, rubato_stereo,
};
pub use trim::{
    trim_codec_priming_drops_leading_frames_and_fades_in,
    trim_codec_priming_metadata_takes_precedence_when_combined, trim_ramp, trim_silence,
};
pub use warp::{warp_clicks, warp_constant, warp_nominal_clicks, warp_pair, warp_sine};
