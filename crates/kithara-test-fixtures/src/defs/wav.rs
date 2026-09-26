#![cfg(feature = "wav")]

use kithara_test_macros as kithara;
use num_traits::cast;

use crate::signal::{Pcm, Wave, header, wav, wav_from_fn, wav_of_size};

mod consts {
    pub(super) const BEATS_PER_BAR: usize = 4;
    pub(super) const BEAT_MARKER_PEAK: i16 = 22_000;
    pub(super) const BEAT_TONE_PEAK: i16 = 10_000;
    pub(super) const CHANNELS: u16 = 2;
    pub(super) const DOWNBEAT_MARKER_PEAK: i16 = 28_000;
    pub(super) const DOWNBEAT_TONE_PEAK: i16 = 14_000;
    pub(super) const LONG_FRAMES: usize = 529_200;
    pub(super) const MARKER_FRAMES: usize = 2_205;
    pub(super) const MARKER_PEAK: i16 = 2_000;
    pub(super) const MARKER_STARTS: [usize; 2] = [17_640, 35_280];
    pub(super) const MILLIS_PER_SECOND: usize = 1_000;
    pub(super) const PULSE_DURATION_MS: usize = 40;
    pub(super) const SAMPLE_RATE: u32 = 44_100;
    pub(super) const SECONDS_PER_MINUTE: f64 = 60.0;
    pub(super) const SHORT_FRAMES: usize = 88_200;
    pub(super) const SOURCE_FRAMES: usize = 264_600;
    pub(super) const TONE_HZ: f64 = 440.0;
    pub(super) const TONE_PEAK: i16 = 16_000;
}

#[derive(Clone, Copy)]
pub(super) enum RhythmControl {
    Aligned,
    BarPhaseBeats(usize),
    MissingBeat(usize),
}

/// Plain 440 Hz tone.
#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::a440_10_frames(10, i16::MAX)]
#[case::a440_100_frames(100, i16::MAX)]
#[case::a440_10000_frames(10_000, i16::MAX)]
#[case::a440_full_scale_2s(consts::SHORT_FRAMES, i16::MAX)]
#[case::a440_2s(consts::SHORT_FRAMES, consts::TONE_PEAK)]
#[case::a440_6s(consts::SOURCE_FRAMES, consts::TONE_PEAK)]
#[case::a440_12s(consts::LONG_FRAMES, consts::TONE_PEAK)]
fn sine_wav(total_frames: usize, peak: i16) -> Vec<u8> {
    wav(
        consts::SAMPLE_RATE,
        consts::CHANNELS,
        total_frames,
        Wave::Sine {
            peak,
            hz: consts::TONE_HZ,
        },
    )
}

/// One sample level held for the whole track: a queue entry whose loudness
/// tells which track is playing.
#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::quiet_0_4s(17_640, 3_276)]
#[case::quiet_1s(44_100, 3_276)]
#[case::quiet_1_5s(66_150, 3_276)]
#[case::quiet_8s(352_800, 3_276)]
#[case::quiet_30s(1_323_000, 3_276)]
#[case::quiet_120s(5_292_000, 3_276)]
#[case::two_1s(44_100, 6_553)]
#[case::three_0_2s(8_820, 9_830)]
#[case::three_0_4s(17_640, 9_830)]
#[case::three_1s(44_100, 9_830)]
#[case::three_5s(220_500, 9_830)]
#[case::three_8s(352_800, 9_830)]
#[case::four_1_5s(66_150, 13_106)]
#[case::loud_0_4s(17_640, 26_213)]
#[case::loud_0_5s(22_050, 26_213)]
#[case::loud_1s(44_100, 26_213)]
#[case::loud_1_5s(66_150, 26_213)]
#[case::loud_8s(352_800, 26_213)]
#[case::loud_30s(1_323_000, 26_213)]
fn constant_wav(total_frames: usize, level: i16) -> Vec<u8> {
    wav_from_fn(consts::SAMPLE_RATE, consts::CHANNELS, total_frames, |_| {
        level
    })
}

/// 440 Hz tone with two lower-amplitude source-time markers.
#[kithara::asset(ext = "wav", content_type = "audio/wav", embed)]
#[case::a440_6s(consts::SOURCE_FRAMES, consts::TONE_PEAK, consts::MARKER_PEAK)]
fn marked_sine_wav(total_frames: usize, peak: i16, marker_peak: i16) -> Vec<u8> {
    wav_from_fn(
        consts::SAMPLE_RATE,
        consts::CHANNELS,
        total_frames,
        |frame| {
            let in_marker = consts::MARKER_STARTS
                .iter()
                .any(|start| frame >= *start && frame < start + consts::MARKER_FRAMES);
            let peak = if in_marker { marker_peak } else { peak };
            Wave::Sine {
                peak,
                hz: consts::TONE_HZ,
            }
            .sample(frame, consts::SAMPLE_RATE)
        },
    )
}

/// Four-beat pulse track with an exact frame-addressable beat marker.
#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::deck_a_120bpm_48k(48_000, 2, 576_000, 120.0, 220.0, 0, RhythmControl::Aligned)]
#[case::deck_b_120bpm_48k(48_000, 2, 576_000, 120.0, 880.0, 0, RhythmControl::Aligned)]
#[case::deck_c_120bpm_48k(48_000, 2, 576_000, 120.0, 1_760.0, 0, RhythmControl::Aligned)]
#[case::deck_d_120bpm_48k(48_000, 2, 576_000, 120.0, 3_520.0, 0, RhythmControl::Aligned)]
#[case::deck_b_one_frame_late_120bpm_48k(
    48_000,
    2,
    576_000,
    120.0,
    880.0,
    1,
    RhythmControl::Aligned
)]
#[case::deck_b_one_beat_bar_late_120bpm_48k(
    48_000,
    2,
    576_000,
    120.0,
    880.0,
    0,
    RhythmControl::BarPhaseBeats(1)
)]
#[case::deck_b_missing_beat_120bpm_48k(
    48_000,
    2,
    576_000,
    120.0,
    880.0,
    0,
    RhythmControl::MissingBeat(5)
)]
fn rhythm_wav(
    sample_rate: u32,
    channels: u16,
    total_frames: usize,
    bpm: f64,
    carrier_hz: f64,
    phase_frame: usize,
    control: RhythmControl,
) -> Vec<u8> {
    let mut bytes = header(
        sample_rate,
        channels,
        Some(total_frames * usize::from(channels) * size_of::<i16>()),
    );
    bytes.extend(Vec::<u8>::from(rhythm_pcm(
        sample_rate,
        channels,
        total_frames,
        bpm,
        carrier_hz,
        phase_frame,
        control,
    )));
    bytes
}

pub(super) fn rhythm_pcm(
    sample_rate: u32,
    channels: u16,
    total_frames: usize,
    bpm: f64,
    carrier_hz: f64,
    phase_frame: usize,
    control: RhythmControl,
) -> Pcm {
    let beat_frames: usize =
        cast((f64::from(sample_rate) * consts::SECONDS_PER_MINUTE / bpm).round())
            .expect("invariant: a fixture beat period fits usize");
    let first_beat = beat_frames + phase_frame;
    let pulse_frames = usize::try_from(sample_rate).expect("invariant: a sample rate fits usize")
        * consts::PULSE_DURATION_MS
        / consts::MILLIS_PER_SECOND;
    let (bar_phase, missing_beat) = match control {
        RhythmControl::Aligned => (0, None),
        RhythmControl::BarPhaseBeats(phase) => (phase, None),
        RhythmControl::MissingBeat(missing) => (0, Some(missing)),
    };

    Pcm::from_fn(sample_rate, channels, total_frames, |frame| {
        let Some(since_first) = frame.checked_sub(first_beat) else {
            return 0;
        };
        let beat = since_first / beat_frames;
        let within_beat = since_first % beat_frames;
        if missing_beat == Some(beat) {
            return 0;
        }
        let downbeat = beat % consts::BEATS_PER_BAR == bar_phase;
        if within_beat == 0 {
            return if downbeat {
                consts::DOWNBEAT_MARKER_PEAK
            } else {
                consts::BEAT_MARKER_PEAK
            };
        }
        if within_beat >= pulse_frames {
            return 0;
        }

        Wave::Sine {
            hz: carrier_hz,
            peak: if downbeat {
                consts::DOWNBEAT_TONE_PEAK
            } else {
                consts::BEAT_TONE_PEAK
            },
        }
        .sample(within_beat, sample_rate)
    })
}

#[kithara::asset(ext = "wav", content_type = "audio/wav")]
#[case::timeline_saw_2mb(2_000_000, Wave::Sawtooth)]
fn sized_wav(total_bytes: usize, wave: Wave) -> Vec<u8> {
    wav_of_size(consts::SAMPLE_RATE, consts::CHANNELS, total_bytes, wave)
}

#[kithara::asset(ext = "wav", content_type = "audio/wav", embed)]
#[case::default()]
fn timeline_wav() -> Vec<u8> {
    sine_wav(441_000, i16::MAX)
}
