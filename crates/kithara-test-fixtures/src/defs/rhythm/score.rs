use std::sync::OnceLock;

use cochlea_render::render_serial;
use cochlea_score::{Bpm, Dur, Insert, Instrument, Pitch, Ppq, SampleRate, Score, Ticks, Vel};
use kithara_analysis::BeatArtifact;
use num_traits::cast;

use crate::signal::header;

struct Consts;

impl Consts {
    const BEATS_PER_BAR: usize = 4;
    const BED_PEAK: f32 = 0.42;
    const CHANNELS: u16 = 2;
    const LOOP_BEATS: usize = 8;
    const MISSING_BEAT: usize = 5;
    const PPQ: u32 = 960;
    const SAMPLE_RATE: u32 = 48_000;
    const SECONDS: usize = 12;
    const SECONDS_PER_MINUTE: f64 = 60.0;
    const TOTAL_FRAMES: usize = 48_000 * Self::SECONDS;
}

static BEDS: [OnceLock<Vec<f32>>; 6] = [const { OnceLock::new() }; 6];

#[derive(Clone, Copy)]
pub(super) enum Control {
    Aligned,
    OneFrameLate,
    OneBeatBarLate,
    MissingBeat,
}

#[derive(Clone, Copy)]
pub(super) enum Style {
    AmbientDub,
    TripHop,
    Downtempo,
    House,
    Techno,
    Breakbeat,
}

#[derive(Clone, Copy)]
pub(super) enum ChannelLayout {
    Stereo,
    LeftOnly,
    RightOnly,
}

#[derive(Clone, Copy)]
pub(super) enum Origin {
    Pickup,
    Zero,
}

impl ChannelLayout {
    const fn frame(self, stereo: [i16; 2]) -> [i16; 2] {
        match self {
            Self::Stereo => stereo,
            Self::LeftOnly => [stereo[0], 0],
            Self::RightOnly => [0, stereo[1]],
        }
    }
}

impl Style {
    const fn bass(self, beat: usize) -> Pitch {
        match self {
            Self::AmbientDub => [Pitch::A1, Pitch::E2][(beat / 4) % 2],
            Self::TripHop => [Pitch::C2, Pitch::G1][(beat / 2) % 2],
            Self::Downtempo => [Pitch::D2, Pitch::A1][(beat / 2) % 2],
            Self::House => [Pitch::E2, Pitch::B1][beat % 2],
            Self::Techno => [Pitch::FS2, Pitch::CS2][(beat / 2) % 2],
            Self::Breakbeat => [Pitch::A2, Pitch::E2][(beat / 2) % 2],
        }
    }

    pub(super) const fn bpm(self) -> f64 {
        match self {
            Self::AmbientDub => 62.0,
            Self::TripHop => 74.0,
            Self::Downtempo => 96.0,
            Self::House => 124.0,
            Self::Techno => 132.0,
            Self::Breakbeat => 140.0,
        }
    }

    const fn harmony(self) -> [Pitch; 3] {
        match self {
            Self::AmbientDub | Self::Breakbeat => [Pitch::A3, Pitch::C4, Pitch::E4],
            Self::TripHop => [Pitch::C4, Pitch::DS4, Pitch::G4],
            Self::Downtempo => [Pitch::D4, Pitch::F4, Pitch::A4],
            Self::House => [Pitch::E4, Pitch::G4, Pitch::B4],
            Self::Techno => [Pitch::FS3, Pitch::A3, Pitch::CS4],
        }
    }

    const fn hat_divisions(self) -> usize {
        match self {
            Self::AmbientDub => 1,
            Self::TripHop | Self::Downtempo | Self::House => 2,
            Self::Techno | Self::Breakbeat => 4,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::AmbientDub => 0,
            Self::TripHop => 1,
            Self::Downtempo => 2,
            Self::House => 3,
            Self::Techno => 4,
            Self::Breakbeat => 5,
        }
    }

    const fn kick(self, beat: usize) -> bool {
        match self {
            Self::AmbientDub => matches!(beat % 4, 0 | 3),
            Self::TripHop => matches!(beat % 8, 0 | 3 | 6),
            Self::Downtempo => matches!(beat % 4, 0 | 2),
            Self::House | Self::Techno => true,
            Self::Breakbeat => matches!(beat % 8, 0 | 2 | 5 | 7),
        }
    }

    const fn music_preset(self) -> &'static str {
        match self {
            Self::AmbientDub => "chord_pad",
            Self::TripHop => "fm_bell",
            Self::Downtempo => "marimba",
            Self::House => "pluck",
            Self::Techno => "saw_lead",
            Self::Breakbeat => "organ",
        }
    }

    const fn snare(self, beat: usize) -> bool {
        match self {
            Self::AmbientDub => beat % 4 == 2,
            Self::TripHop => matches!(beat % 8, 2 | 6),
            Self::Downtempo | Self::House | Self::Techno => matches!(beat % 4, 1 | 3),
            Self::Breakbeat => matches!(beat % 8, 1 | 4 | 6),
        }
    }
}

pub(super) struct Truth {
    beats: Vec<(u64, Option<f32>)>,
    downbeats: Vec<(u64, Option<f32>)>,
    bpm: f64,
}

impl From<Truth> for BeatArtifact {
    fn from(truth: Truth) -> Self {
        Self::new(truth.bpm, truth.beats, truth.downbeats)
    }
}

pub(super) fn wav(style: Style, control: Control) -> Vec<u8> {
    wav_with_layout(style, control, ChannelLayout::Stereo)
}

pub(super) fn wav_with_layout(style: Style, control: Control, layout: ChannelLayout) -> Vec<u8> {
    wav_with_layout_at_origin(style, control, layout, Origin::Pickup)
}

pub(super) fn wav_with_layout_at_origin(
    style: Style,
    control: Control,
    layout: ChannelLayout,
    origin: Origin,
) -> Vec<u8> {
    wav_with_layout_at_origin_for_frames(
        style,
        control,
        layout,
        origin,
        Consts::TOTAL_FRAMES as u64,
    )
}

pub(super) fn wav_with_layout_at_origin_for_frames(
    style: Style,
    control: Control,
    layout: ChannelLayout,
    origin: Origin,
    total_frames: u64,
) -> Vec<u8> {
    let total_frames = usize::try_from(total_frames).expect("fixture frame count fits usize");
    let pcm = pcm(style, control, layout, origin, total_frames);
    let mut bytes = header(Consts::SAMPLE_RATE, Consts::CHANNELS, Some(pcm.len()));
    bytes.extend(pcm);
    bytes
}

pub(super) fn truth(style: Style, control: Control) -> Truth {
    truth_at_origin(style, control, Origin::Pickup)
}

pub(super) fn truth_at_origin(style: Style, control: Control, origin: Origin) -> Truth {
    truth_at_origin_for_frames(style, control, origin, Consts::TOTAL_FRAMES as u64)
}

pub(super) fn truth_at_origin_for_frames(
    style: Style,
    control: Control,
    origin: Origin,
    total_frames: u64,
) -> Truth {
    let total_frames = usize::try_from(total_frames).expect("fixture frame count fits usize");
    let beat_frames = beat_frames(style);
    let first = first_beat_frame(beat_frames, control, origin);
    let bar_phase = usize::from(matches!(control, Control::OneBeatBarLate));
    let mut beats = Vec::new();
    let mut downbeats = Vec::new();
    for (beat, frame) in (first..total_frames).step_by(beat_frames).enumerate() {
        if matches!(control, Control::MissingBeat) && beat == Consts::MISSING_BEAT {
            continue;
        }
        let mark = (u64::try_from(frame).unwrap_or(u64::MAX), Some(1.0));
        beats.push(mark);
        if beat % Consts::BEATS_PER_BAR == bar_phase {
            downbeats.push(mark);
        }
    }
    Truth {
        beats,
        downbeats,
        bpm: style.bpm(),
    }
}

fn pcm(
    style: Style,
    control: Control,
    layout: ChannelLayout,
    origin: Origin,
    total_frames: usize,
) -> Vec<u8> {
    let beat_frames = beat_frames(style);
    let first = first_beat_frame(beat_frames, control, origin);
    let bed = bed(style, beat_frames);
    let mut bytes =
        Vec::with_capacity(total_frames * usize::from(Consts::CHANNELS) * size_of::<i16>());

    for frame in 0..total_frames {
        let Some(since_first) = frame.checked_sub(first) else {
            push_frame(&mut bytes, layout.frame([0, 0]));
            continue;
        };
        let beat = since_first / beat_frames;
        let within = since_first % beat_frames;
        let missing = matches!(control, Control::MissingBeat) && beat == Consts::MISSING_BEAT;
        if missing {
            push_frame(&mut bytes, layout.frame([0, 0]));
            continue;
        }

        let bar_phase = usize::from(matches!(control, Control::OneBeatBarLate));
        let pattern_beat = beat + Consts::BEATS_PER_BAR - bar_phase;
        if within == 0 {
            let marker = if pattern_beat.is_multiple_of(Consts::BEATS_PER_BAR) {
                28_000
            } else {
                22_000
            };
            push_frame(&mut bytes, layout.frame([marker, marker]));
            continue;
        }

        let phase_offset = if bar_phase == 0 {
            0
        } else {
            (Consts::BEATS_PER_BAR - 1) * beat_frames
        };
        let loop_frames = Consts::LOOP_BEATS * beat_frames;
        let source = ((since_first + phase_offset) % loop_frames) * usize::from(Consts::CHANNELS);
        let frame = bed
            .get(source..source + usize::from(Consts::CHANNELS))
            .map_or([0, 0], |samples| {
                [pcm_sample(samples[0]), pcm_sample(samples[1])]
            });
        push_frame(&mut bytes, layout.frame(frame));
    }
    bytes
}

fn bed(style: Style, beat_frames: usize) -> &'static [f32] {
    BEDS[style.index()]
        .get_or_init(|| render_bed(style, beat_frames))
        .as_slice()
}

fn render_bed(style: Style, beat_frames: usize) -> Vec<f32> {
    let mut score = Score::new(SampleRate(Consts::SAMPLE_RATE), Ppq(Consts::PPQ))
        .tempo(Ticks::ZERO, Bpm(style.bpm()))
        .track("kick", Instrument::preset("kick"))
        .track("snare", Instrument::preset("snare"))
        .track("hat", Instrument::preset("noise_hat"))
        .track("bass", Instrument::preset("square_bass"))
        .track("music", Instrument::preset(style.music_preset()))
        .insert("music", Insert::preset("reverb"));

    for beat in 0..Consts::LOOP_BEATS {
        score = add_beat(score, style, beat);
    }

    let rendered = render_serial(&score)
        .unwrap_or_else(|error| panic!("render {:?} BPM fixture: {error}", style.bpm()));
    let mut mix = rendered.mix().to_vec();
    let peak = mix
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    let gain = Consts::BED_PEAK / peak.max(Consts::BED_PEAK);
    for sample in &mut mix {
        *sample *= gain;
    }
    let frames = Consts::LOOP_BEATS * beat_frames;
    mix.resize(frames * usize::from(Consts::CHANNELS), 0.0);
    mix
}

fn add_beat(mut score: Score, style: Style, beat: usize) -> Score {
    let at = beat_at(beat, 0, 1);
    if style.kick(beat) {
        score = score.note("kick", at, Dur::quarter(), Pitch::C1, Vel(112));
    }
    if style.snare(beat) {
        score = score.note("snare", at, Dur::eighth(), Pitch::D3, Vel(92));
    }
    for division in 0..style.hat_divisions() {
        let velocity = if division.is_multiple_of(2) { 68 } else { 48 };
        score = score.note(
            "hat",
            beat_at(beat, division, style.hat_divisions()),
            Dur::sixteenth(),
            Pitch::A5,
            Vel(velocity),
        );
    }

    score = add_bass(score, style, beat);
    add_music(score, style, beat)
}

fn add_bass(score: Score, style: Style, beat: usize) -> Score {
    let (play, division, divisions, duration) = match style {
        Style::AmbientDub => (matches!(beat % 4, 0 | 3), 0, 1, Dur::half()),
        Style::TripHop => (matches!(beat % 8, 0 | 3 | 6), 0, 1, Dur::quarter()),
        Style::Downtempo => (beat.is_multiple_of(2), 0, 1, Dur::quarter()),
        Style::House => (true, 1, 2, Dur::eighth()),
        Style::Techno => (beat.is_multiple_of(2), 0, 1, Dur::eighth()),
        Style::Breakbeat => (matches!(beat % 8, 0 | 2 | 5 | 7), 0, 1, Dur::eighth()),
    };
    if play {
        score.note(
            "bass",
            beat_at(beat, division, divisions),
            duration,
            style.bass(beat),
            Vel(88),
        )
    } else {
        score
    }
}

fn add_music(mut score: Score, style: Style, beat: usize) -> Score {
    let harmony = style.harmony();
    match style {
        Style::AmbientDub if beat.is_multiple_of(4) => {
            for pitch in harmony {
                score = score.note("music", beat_at(beat, 0, 1), Dur::whole(), pitch, Vel(54));
            }
        }
        Style::TripHop if matches!(beat % 8, 1 | 4 | 7) => {
            score = score.note(
                "music",
                beat_at(beat, 0, 1),
                Dur::eighth(),
                harmony[beat % harmony.len()],
                Vel(66),
            );
        }
        Style::Downtempo if beat.is_multiple_of(2) => {
            score = score.note(
                "music",
                beat_at(beat, 0, 1),
                Dur::quarter(),
                harmony[(beat / 2) % harmony.len()],
                Vel(70),
            );
        }
        Style::House => {
            score = score.note(
                "music",
                beat_at(beat, 1, 2),
                Dur::eighth(),
                harmony[beat % harmony.len()],
                Vel(68),
            );
        }
        Style::Techno => {
            for division in [1, 3] {
                score = score.note(
                    "music",
                    beat_at(beat, division, 4),
                    Dur::sixteenth(),
                    harmony[(beat + division) % harmony.len()],
                    Vel(58),
                );
            }
        }
        Style::Breakbeat if beat.is_multiple_of(2) => {
            for pitch in harmony {
                score = score.note("music", beat_at(beat, 0, 1), Dur::half(), pitch, Vel(48));
            }
        }
        _ => {}
    }
    score
}

fn beat_at(beat: usize, division: usize, divisions: usize) -> Ticks {
    let beat = u64::try_from(beat).expect("invariant: fixture beat index fits u64");
    let division = u64::try_from(division).expect("invariant: fixture division fits u64");
    let divisions = u64::try_from(divisions).expect("invariant: fixture divisions fit u64");
    let ppq = u64::from(Consts::PPQ);
    Ticks(beat * ppq + division * (ppq / divisions))
}

fn push_frame(bytes: &mut Vec<u8>, frame: [i16; 2]) {
    for sample in frame {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
}

fn pcm_sample(sample: f32) -> i16 {
    cast((f64::from(sample.clamp(-1.0, 1.0)) * f64::from(i16::MAX)).round()).unwrap_or(0)
}

fn beat_frames(style: Style) -> usize {
    cast((f64::from(Consts::SAMPLE_RATE) * Consts::SECONDS_PER_MINUTE / style.bpm()).round())
        .expect("invariant: a rhythm beat period fits usize")
}

const fn first_beat_frame(beat_frames: usize, control: Control, origin: Origin) -> usize {
    let pickup = match origin {
        Origin::Pickup => beat_frames,
        Origin::Zero => 0,
    };
    pickup + phase_frames(control)
}

const fn phase_frames(control: Control) -> usize {
    match control {
        Control::OneFrameLate => 1,
        Control::Aligned | Control::OneBeatBarLate | Control::MissingBeat => 0,
    }
}
