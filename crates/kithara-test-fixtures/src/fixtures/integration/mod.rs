mod encoded;
#[cfg(not(target_arch = "wasm32"))]
mod native;
mod oracles;
mod pcm;

pub use encoded::{
    audio_wav_8000, audio_wav_44100, audio_wav_132300, audio_wav_176400, audio_wav_264600,
    audio_wav_1323000, concurrent_wav, drain_tone, encoder_saw_aac, encoder_saw_flac,
    encoder_saw_he, encoder_second, flac_config, perf_wav, saw_segments,
};
#[cfg(not(target_arch = "wasm32"))]
pub use native::{
    benchmark_half, constant_four, constant_half, constant_loud, constant_quarter, constant_quiet,
    constant_three, constant_two, constant_unity, deadline_tracks,
};
pub use oracles::{
    cochlea_control, cochlea_loudness, listening_reference, oracle_stem_a, oracle_stem_b,
    phase_noise, phase_sine_anchor, phase_sine_dropped, phase_sine_jitter, phase_sine_measured,
    quality_control_a, quality_control_b, quality_control_joined, rms_silence, rms_unit,
    shifted_pitch,
};
pub use pcm::{
    allocation_planar, allocation_ramp, allocation_sequence, broadcast_tone, default_pcm,
    dsp_silence, dsp_sweep, dsp_tone_a220, dsp_tone_a440, dsp_tone_ceiling, dsp_tone_ceiling_low,
    dsp_tone_ceiling_unity, dsp_tone_round_low, dsp_tone_round_mid, dsp_tone_round_poison,
    dsp_tone_unity, gapless_sine_first, gapless_sine_second, gapless_sine_whole, origin_tone,
    packaging_tone, perf_interleaved, perf_planar, stream_sine,
};
