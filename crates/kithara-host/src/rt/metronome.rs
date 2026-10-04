use core::{
    f64::consts::{PI, TAU},
    num::NonZeroU32,
};

use firewheel::{
    StreamInfo,
    channel_config::{ChannelConfig, ChannelCount},
    diff::{Diff, Patch},
    event::ProcEvents,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        NodeError, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use kithara_config::Config;
use kithara_platform::time::Duration;
use kithara_play::rt::read_render_context;
use kithara_signal::SessionFrame;
use kithara_test_utils::kithara;
use kithara_warp::{SessionAnchor, SessionBeat};
use num_traits::ToPrimitive;

use crate::PlayError;

mod consts {
    use kithara_platform::time::Duration;

    /// Tone of a beat click.
    pub(super) const BEAT_HZ: f64 = 1_760.0;
    /// Tone of a downbeat click.
    pub(super) const DOWNBEAT_HZ: f64 = 2_200.0;
    /// Rise of a click and of its duck from silence to their peak. A
    /// raised-cosine rise and fall keep the click and its duck band-limited.
    /// Between samples the duck's modulation still folds the mix's content
    /// near Nyquist back over the limiter's true-peak ceiling; the rise sets
    /// how much, the fall, hold and release add nothing. Measured at the
    /// worst phase with a tone at the ceiling at 44.1 kHz under a full duck
    /// and a click at the ceiling: about a ten-thousandth of a decibel at DC,
    /// under a thousandth up to 16 kHz, under a hundredth up to 20 kHz, under
    /// four hundredths up to 21 kHz, and about 3.7 dB at 22 kHz.
    pub(super) const ATTACK_SECONDS: f64 = 0.002;
    /// The session transport counts bars of four beats from session beat 0.
    pub(super) const BEATS_PER_BAR: i64 = 4;
    /// Peak of a beat click relative to a downbeat click.
    pub(super) const BEAT_RATIO: f64 = 0.625;
    /// A downbeat click peaks at the limiter ceiling.
    pub(super) const DEFAULT_LEVEL: f32 = 1.0;
    /// The duck mutes the mix under every click.
    pub(super) const DEFAULT_DUCK: f32 = 1.0;
    pub(super) const DEFAULT_DECAY: Duration = Duration::from_millis(35);
    pub(super) const DEFAULT_HOLD: Duration = Duration::from_millis(20);
    pub(super) const DEFAULT_RELEASE: Duration = Duration::from_millis(80);
    pub(super) const MIN_DECAY: Duration = Duration::from_millis(8);
    pub(super) const MAX_DECAY: Duration = Duration::from_millis(50);
    pub(super) const MAX_HOLD: Duration = Duration::from_secs(1);
    pub(super) const MIN_RELEASE: Duration = Duration::from_millis(8);
    pub(super) const MAX_RELEASE: Duration = Duration::from_secs(1);
}

/// How the Host metronome sounds. A click is a sine tone under a 2 ms
/// raised-cosine rise and a raised-cosine fall; under it the mix is ducked
/// by the same rise, held down through the fall and the hold, and returned
/// by a raised-cosine release. A duck at least as deep as the level keeps
/// the ducked mix plus the click under the limiter ceiling at every sample;
/// a shallower duck, or none, lets a loud mix plus the click pass it.
///
/// The builder takes any values; the Host checks them when it starts and
/// refuses a config out of the bounds each field names with
/// [`PlayError::InvalidParameter`], naming `metronome_level`,
/// `metronome_duck`, `metronome_decay`, `metronome_hold` or
/// `metronome_release`.
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(
    default,
    builder(state_mod(vis = "pub")),
    check(error = PlayError),
    fields(value, get(copy))
)]
#[non_exhaustive]
pub struct MetronomeConfig {
    /// Peak of a downbeat click as a share of the limiter ceiling, above
    /// zero and at most one; a beat click peaks at five eighths of it.
    #[config(live, check = Self::level_bounds, builder(default = consts::DEFAULT_LEVEL))]
    level: f32,
    /// Share of the mix the duck takes away while the click sounds, from
    /// zero to one: one mutes the mix, zero leaves it whole.
    #[config(check = Self::duck_bounds, builder(default = consts::DEFAULT_DUCK))]
    duck: f32,
    /// Fall of a click from its peak back to silence, 8 to 50 ms.
    #[config(check = Self::decay_bounds, builder(default = consts::DEFAULT_DECAY))]
    decay: Duration,
    /// How long the duck keeps the mix down after the click has fallen, at
    /// most 1 s.
    #[config(check = Self::hold_bounds, builder(default = consts::DEFAULT_HOLD))]
    hold: Duration,
    /// How long the duck takes to return the mix after its hold, 8 ms to 1 s.
    #[config(check = Self::release_bounds, builder(default = consts::DEFAULT_RELEASE))]
    release: Duration,
}

impl MetronomeConfig {
    fn level_bounds(level: f32) -> Result<f32, PlayError> {
        bounded(level > 0.0 && level <= 1.0, "metronome_level", level, level)
    }

    fn duck_bounds(duck: f32) -> Result<f32, PlayError> {
        bounded((0.0..=1.0).contains(&duck), "metronome_duck", duck, duck)
    }

    fn decay_bounds(decay: Duration) -> Result<Duration, PlayError> {
        let valid = (consts::MIN_DECAY..=consts::MAX_DECAY).contains(&decay);
        bounded(valid, "metronome_decay", decay.as_secs_f32(), decay)
    }

    fn hold_bounds(hold: Duration) -> Result<Duration, PlayError> {
        bounded(
            hold <= consts::MAX_HOLD,
            "metronome_hold",
            hold.as_secs_f32(),
            hold,
        )
    }

    fn release_bounds(release: Duration) -> Result<Duration, PlayError> {
        let valid = (consts::MIN_RELEASE..=consts::MAX_RELEASE).contains(&release);
        bounded(valid, "metronome_release", release.as_secs_f32(), release)
    }
}

/// `field` if it is `valid`, or the refusal naming the parameter and its
/// value.
fn bounded<T>(valid: bool, name: &str, value: f32, field: T) -> Result<T, PlayError> {
    if valid {
        Ok(field)
    } else {
        Err(PlayError::InvalidParameter {
            name: name.to_owned(),
            value,
        })
    }
}

/// The limiter ceiling a click's level is a share of, the duck's depth, and
/// the click's fall, hold and release in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shape {
    ceiling: f64,
    depth: f64,
    decay: f64,
    hold: f64,
    release: f64,
}

/// The Host metronome between the limiter and `graph_out`: a click on every
/// session beat while enabled and the transport runs.
#[derive(Diff, Patch, Debug, Clone, Copy, PartialEq)]
pub(crate) struct MetronomeNode {
    pub(crate) enabled: bool,
    /// Peak of a downbeat click as a share of the limiter ceiling.
    pub(crate) level: f32,
    #[diff(skip)]
    shape: Shape,
}

impl MetronomeNode {
    pub(crate) fn new(enabled: bool, config: MetronomeConfig, ceiling: f32) -> Self {
        Self {
            enabled,
            level: config.level,
            shape: Shape {
                ceiling: f64::from(ceiling),
                depth: f64::from(config.duck),
                decay: config.decay.as_secs_f64(),
                hold: config.hold.as_secs_f64(),
                release: config.release.as_secs_f64(),
            },
        }
    }
}

/// A click's rise, fall, hold and release in frames at one rate.
struct Envelope {
    attack: f64,
    decay: f64,
    hold: f64,
    release: f64,
}

impl Envelope {
    fn new(shape: Shape, rate: f64) -> Self {
        Self {
            attack: (consts::ATTACK_SECONDS * rate).round().max(1.0),
            decay: (shape.decay * rate).round().max(1.0),
            hold: (shape.hold * rate).round(),
            release: (shape.release * rate).round().max(1.0),
        }
    }

    /// Frames from a click's start to the end of its duck's release.
    fn frames(&self) -> f64 {
        self.attack + self.decay + self.hold + self.release
    }

    /// The tone's and the duck's envelope `elapsed` frames into a click
    /// whose duck rises from `from`. The duck rises with the tone and never
    /// under it, holds full through the tone's fall and the hold, then
    /// releases.
    fn at(&self, elapsed: f64, from: f64) -> (f64, f64) {
        let fallen = elapsed - self.attack;
        let released = fallen - self.decay - self.hold;
        if elapsed < self.attack {
            let rise = 0.5 * (1.0 - (PI * elapsed / self.attack).cos());
            (rise, from.mul_add(1.0 - rise, rise))
        } else if fallen < self.decay {
            (0.5 * (1.0 + (PI * fallen / self.decay).cos()), 1.0)
        } else if released < 0.0 {
            (0.0, 1.0)
        } else if released < self.release {
            (0.0, 0.5 * (1.0 + (PI * released / self.release).cos()))
        } else {
            (0.0, 0.0)
        }
    }
}

/// One sounding click: a sine tone under its envelope, ducking the mix by
/// the duck's envelope.
#[derive(Clone, Copy, Debug)]
struct Click {
    elapsed: f64,
    rate: f64,
    hz: f64,
    peak: f64,
    /// The duck the click takes over from the click it cuts off.
    from: f64,
    shape: Shape,
}

impl Click {
    fn new(downbeat: bool, sample_rate: NonZeroU32, level: f32, shape: Shape, from: f64) -> Self {
        // WHY: The product of two `f32` values is exact in `f64`, so a click
        // at a level of one peaks exactly at the ceiling.
        let peak = f64::from(level) * shape.ceiling;
        let (hz, peak) = if downbeat {
            (consts::DOWNBEAT_HZ, peak)
        } else {
            (consts::BEAT_HZ, peak * consts::BEAT_RATIO)
        };
        Self {
            elapsed: 0.0,
            rate: f64::from(sample_rate.get()),
            hz,
            peak,
            from,
            shape,
        }
    }

    /// Keeps the click's elapsed time, not its frame count, across a rate change.
    fn retune(&mut self, sample_rate: NonZeroU32) {
        let rate = f64::from(sample_rate.get());
        self.elapsed *= rate / self.rate;
        self.rate = rate;
    }

    /// The duck on the click's next frame.
    fn duck(&self) -> f64 {
        Envelope::new(self.shape, self.rate)
            .at(self.elapsed, self.from)
            .1
    }

    /// Ducks `left`/`right` under the click and adds it. Returns whether the
    /// click still sounds or ducks after these frames.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        let envelope = Envelope::new(self.shape, self.rate);
        let frames = envelope.frames();
        let cycles_per_frame = self.hz / self.rate;
        for (l, r) in left.iter_mut().zip(right) {
            if self.elapsed >= frames {
                break;
            }
            let phase = (self.elapsed * cycles_per_frame).fract();
            let (tone, duck) = envelope.at(self.elapsed, self.from);
            self.elapsed += 1.0;
            let gain = (1.0 - self.shape.depth * duck).to_f32().unwrap_or_default();
            let click = (self.peak * (TAU * phase).sin() * tone)
                .to_f32()
                .unwrap_or_default();
            *l = l.mul_add(gain, click);
            *r = r.mul_add(gain, click);
        }
        self.elapsed < frames
    }
}

#[derive(Debug, Default)]
struct Metronome {
    click: Option<Click>,
    /// The next session beat owed a click while the transport runs on from
    /// [`Self::reached`]. A beat is owed by its ordinal, not by the frame it
    /// rounds to: a route restart rounds the beats on the new rate's frame
    /// grid, which can move a beat across the restart frame in either
    /// direction.
    owed: Option<i64>,
    /// The session beat the last rendered block ended on. A block starting
    /// anywhere else follows a seek or a stretch the metronome did not
    /// render, so no beat before it is owed.
    reached: Option<SessionBeat>,
}

impl Metronome {
    const fn sounding(&self) -> bool {
        self.click.is_some()
    }

    fn retune(&mut self, sample_rate: NonZeroU32) {
        if let Some(click) = self.click.as_mut() {
            click.retune(sample_rate);
        }
    }

    /// Carries the sounding click and its duck over `left`/`right`; with
    /// none, the mix passes untouched.
    fn continue_click(&mut self, left: &mut [f32], right: &mut [f32]) {
        if let Some(click) = self.click.as_mut()
            && !click.render(left, right)
        {
            self.click = None;
        }
    }

    /// Renders one block starting at session frame `start`: the sounding
    /// click first, then a new click on every owed session beat whose frame
    /// is before the block's end, taking over the duck of the click it cuts
    /// off. A block continuing the last one owes the beats from its owed
    /// beat on, clicking one a restart rounded behind the block on its first
    /// frame; any other block owes the beats from its start on. Returns
    /// whether the block was touched.
    fn render(
        &mut self,
        trajectory: Option<SessionAnchor>,
        start: SessionFrame,
        level: f32,
        shape: Shape,
        left: &mut [f32],
        right: &mut [f32],
    ) -> bool {
        let sounding = self.sounding();
        let mut cursor = 0;
        let mut started = false;
        if let Some(anchor) = trajectory {
            let first = i64::from(start);
            let continues = self
                .reached
                .is_some_and(|reached| anchor.frame_at(reached).is_ok_and(|frame| frame == start));
            let owed = self.owed.filter(|_| continues);
            let mut ordinal = owed.or_else(|| {
                anchor
                    .beat_at(start)
                    .ok()
                    .and_then(|beat| f64::from(beat).floor().to_i64())
            });
            while let Some(beat) = ordinal {
                let Some(offset) = beat
                    .to_f64()
                    .and_then(|whole| SessionBeat::new(whole).ok())
                    .and_then(|whole| anchor.frame_at(whole).ok())
                    .and_then(|frame| i64::from(frame).checked_sub(first))
                else {
                    break;
                };
                let offset = match usize::try_from(offset) {
                    Ok(offset) => offset,
                    Err(_) if owed.is_some() => 0,
                    Err(_) => {
                        ordinal = beat.checked_add(1);
                        continue;
                    }
                };
                if offset >= left.len() {
                    break;
                }
                ordinal = beat.checked_add(1);
                let (Some(left_run), Some(right_run)) =
                    (left.get_mut(cursor..offset), right.get_mut(cursor..offset))
                else {
                    break;
                };
                self.continue_click(left_run, right_run);
                let from = self.click.as_ref().map_or(0.0, Click::duck);
                self.click = Some(Click::new(
                    beat.rem_euclid(consts::BEATS_PER_BAR) == 0,
                    anchor.sample_rate(),
                    level,
                    shape,
                    from,
                ));
                cursor = offset;
                started = true;
            }
            self.owed = ordinal;
            let end = i64::try_from(left.len())
                .ok()
                .and_then(|frames| first.checked_add(frames));
            self.reached = end.and_then(|end| anchor.beat_at(SessionFrame::new(end)).ok());
        }
        if let (Some(left_rest), Some(right_rest)) =
            (left.get_mut(cursor..), right.get_mut(cursor..))
        {
            self.continue_click(left_rest, right_rest);
        }
        sounding || started
    }
}

impl AudioNode for MetronomeNode {
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        _cx: ConstructProcessorContext,
    ) -> Result<impl AudioNodeProcessor, NodeError> {
        Ok(MetronomeProcessor {
            params: *self,
            metronome: Metronome::default(),
        })
    }

    fn info(&self, _config: &Self::Configuration) -> Result<AudioNodeInfo, NodeError> {
        Ok(AudioNodeInfo::new()
            .debug_name("session_metronome")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::STEREO,
                num_outputs: ChannelCount::STEREO,
            }))
    }
}

struct MetronomeProcessor {
    params: MetronomeNode,
    metronome: Metronome,
}

impl AudioNodeProcessor for MetronomeProcessor {
    #[kithara::rtsan_forbid_blocking]
    fn events(&mut self, _info: &ProcInfo, events: &mut ProcEvents, _extra: &mut ProcExtra) {
        for patch in events.drain_patches::<MetronomeNode>() {
            self.params.apply(patch);
        }
    }

    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.metronome.retune(stream_info.sample_rate);
    }

    #[kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        extra: &mut ProcExtra,
    ) -> ProcessStatus {
        let trajectory = if self.params.enabled {
            read_render_context(&extra.store, info)
                .ok()
                .and_then(|context| context.trajectory().copied())
        } else {
            None
        };
        if trajectory.is_none() && !self.metronome.sounding() {
            return ProcessStatus::Bypass;
        }
        let frames = info.frames;
        let ([in_left, in_right, ..], [out_left, out_right, ..]) =
            (buffers.inputs, buffers.outputs)
        else {
            return ProcessStatus::Bypass;
        };
        let (Some(in_left), Some(in_right), Some(out_left), Some(out_right)) = (
            in_left.get(..frames),
            in_right.get(..frames),
            out_left.get_mut(..frames),
            out_right.get_mut(..frames),
        ) else {
            return ProcessStatus::Bypass;
        };
        out_left.copy_from_slice(in_left);
        out_right.copy_from_slice(in_right);
        self.metronome.render(
            trajectory,
            SessionFrame::new(info.clock_samples.0),
            self.params.level,
            self.params.shape,
            out_left,
            out_right,
        );
        ProcessStatus::OutputsModified
    }
}

#[cfg(test)]
mod tests {
    use core::f32::consts::PI;

    use kithara_config::{CheckedConfig, LiveConfig};
    use kithara_effects::{LimiterConfig, mock::reconstructed_peak};

    use super::*;

    fn rate() -> NonZeroU32 {
        NonZeroU32::new(44_100).expect("test rate")
    }

    /// The default limiter ceiling a Host's metronome clicks under.
    fn ceiling() -> f32 {
        LimiterConfig::default().ceiling()
    }

    fn config(level: f32, duck: f32) -> MetronomeConfig {
        MetronomeConfig::builder().level(level).duck(duck).build()
    }

    #[kithara::test]
    fn a_duck_never_lifts_a_ceiling_signal_over_the_ceiling() {
        const CEILING: f32 = 0.25;
        const FRAMES: usize = 8_192;

        // WHY: A full duck under a click at the ceiling, the shallowest duck
        // that keeps a level under the ceiling, and a click rising in the
        // release of the last.
        for (level, duck) in [(1.0, 1.0), (0.8, 0.8)] {
            let node = MetronomeNode::new(true, config(level, duck), CEILING);
            for from in [0.0, 0.5] {
                for downbeat in [true, false] {
                    for mix in [CEILING, -CEILING] {
                        let mut left = [mix; FRAMES];
                        let mut right = [mix; FRAMES];
                        Click::new(downbeat, rate(), node.level, node.shape, from)
                            .render(&mut left, &mut right);
                        let bound = CEILING * (1.0 + 4.0 * f32::EPSILON);
                        assert!(
                            left.iter()
                                .chain(&right)
                                .all(|sample| sample.abs() <= bound),
                            "a click at {level} ducking {duck} from {from} over a {mix} mix stays under the ceiling"
                        );
                        assert!(
                            left.iter().any(|sample| *sample != mix),
                            "the click sounds over the mix"
                        );
                    }
                }
            }
        }
    }

    #[kithara::test]
    fn a_click_holds_the_true_peak_to_the_documented_bound() {
        // WHY: The overshoot documented on `consts::ATTACK_SECONDS` for mix
        // content up to 16 kHz. Under a full duck a mix held at the ceiling
        // reconstructs about a ten-thousandth of a decibel over it.
        const DOCUMENTED_OVER_DB: f32 = 0.001;
        const RISE_FRAMES: f32 = 512.0;
        const ONSET: usize = 1_024;
        const FRAMES: u16 = 8_192;
        // WHY: A moving mix under a full-depth duck, far enough under Nyquist
        // that the duck's modulation folds nothing back over the ceiling.
        const TONE_HZ: f64 = 10_000.0;
        const TONE_PHASE: f64 = 2.1;

        let held: Vec<f32> = (0..FRAMES)
            .map(|frame| {
                let rise = (f32::from(frame) / RISE_FRAMES).min(1.0);
                ceiling() * 0.5 * (1.0 - (PI * rise).cos())
            })
            .collect();
        let tone: Vec<f32> = held
            .iter()
            .zip(0..FRAMES)
            .map(|(level, frame)| {
                let cycles = (f64::from(frame) * TONE_HZ / f64::from(rate().get())).fract();
                let tone = f64::from(*level) * TAU.mul_add(cycles, TONE_PHASE).cos();
                tone.to_f32().expect("a tone sample fits f32")
            })
            .collect();
        let sharpest = MetronomeConfig::builder()
            .decay(Duration::from_millis(8))
            .hold(Duration::ZERO)
            .release(Duration::from_millis(8))
            .build();
        for (mix, config) in [
            (vec![0.0; held.len()], MetronomeConfig::default()),
            (held.clone(), MetronomeConfig::default()),
            (held.clone(), config(0.5, 0.5)),
            (tone, MetronomeConfig::default()),
            (held, sharpest),
        ] {
            let node = MetronomeNode::new(true, config, ceiling());
            for downbeat in [true, false] {
                let mut left = mix.clone();
                let mut right = mix.clone();
                Click::new(downbeat, rate(), node.level, node.shape, 0.0).render(
                    left.get_mut(ONSET..).expect("onset inside the mix"),
                    right.get_mut(ONSET..).expect("onset inside the mix"),
                );
                let peak = reconstructed_peak(&left);
                let over_db = 20.0 * (peak / ceiling()).log10();
                assert!(
                    over_db <= DOCUMENTED_OVER_DB,
                    "a click from {config:?} reconstructs to {peak}, {over_db} dB over the ceiling"
                );
            }
        }
    }

    /// A 120 BPM transport at 44.1 kHz playing session beat `beat` on
    /// session frame `frame`: a beat every 22 050 frames.
    fn transport(frame: i64, beat: f64) -> SessionAnchor {
        const BEATS_PER_SECOND: f64 = 2.0;
        SessionAnchor::new(
            SessionFrame::new(frame),
            SessionBeat::new(beat).expect("finite beat"),
            BEATS_PER_SECOND,
            rate(),
        )
        .expect("a positive tempo")
    }

    /// A metronome that clicked session beat 1 and rendered its click and
    /// its duck out: one block of silence around the beat. Returns the frame
    /// after it.
    fn past_beat_one(metronome: &mut Metronome, node: MetronomeNode) -> i64 {
        const LEAD: i64 = 256;
        const FRAMES: usize = 8_192;
        let beat_one = 22_050;
        let start = beat_one - LEAD;
        let mut left = [0.0; FRAMES];
        let mut right = [0.0; FRAMES];
        metronome.render(
            Some(transport(0, 0.0)),
            SessionFrame::new(start),
            node.level,
            node.shape,
            &mut left,
            &mut right,
        );
        assert!(
            left.iter().any(|sample| *sample != 0.0) && !metronome.sounding(),
            "beat 1 clicks and its duck ends inside the block"
        );
        start + i64::try_from(FRAMES).expect("block length")
    }

    #[kithara::test]
    fn a_seek_forward_clicks_none_of_the_beats_it_jumps_over() {
        const TARGET: f64 = 40.25;
        let node = MetronomeNode::new(true, MetronomeConfig::default(), ceiling());
        let mut metronome = Metronome::default();
        let seek = past_beat_one(&mut metronome, node);

        let mut left = [0.0; 1_024];
        let mut right = [0.0; 1_024];
        let touched = metronome.render(
            Some(transport(seek, TARGET)),
            SessionFrame::new(seek),
            node.level,
            node.shape,
            &mut left,
            &mut right,
        );

        assert!(
            !touched && left.iter().chain(&right).all(|sample| *sample == 0.0),
            "no beat lies between the seek target and the block's end"
        );
    }

    #[kithara::test]
    fn a_seek_backward_clicks_the_beats_it_plays_again() {
        const TARGET: f64 = 0.99;
        let node = MetronomeNode::new(true, MetronomeConfig::default(), ceiling());
        let mut metronome = Metronome::default();
        let seek = past_beat_one(&mut metronome, node);
        let anchor = transport(seek, TARGET);
        let beat_one = anchor
            .frame_at(SessionBeat::new(1.0).expect("whole beat"))
            .expect("beat 1 after the seek");
        let offset = usize::try_from(i64::from(beat_one) - seek).expect("beat 1 inside the block");

        let mut left = [0.0; 1_024];
        let mut right = [0.0; 1_024];
        metronome.render(
            Some(anchor),
            SessionFrame::new(seek),
            node.level,
            node.shape,
            &mut left,
            &mut right,
        );

        assert_eq!(
            left.iter().position(|sample| *sample != 0.0),
            Some(offset + 1),
            "beat 1 clicks again, rising from its frame after the seek"
        );
    }

    #[kithara::test]
    fn a_click_in_the_release_of_the_last_takes_its_duck_over_without_a_jump() {
        const START: i64 = 21_794;
        const FRAMES: usize = 44_100;
        const TOLERANCE: f32 = 1e-6;
        let config = MetronomeConfig::builder()
            .hold(Duration::ZERO)
            .release(Duration::from_secs(1))
            .build();
        let node = MetronomeNode::new(true, config, ceiling());
        let render = |mix: f32| {
            let mut left = vec![mix; FRAMES];
            let mut right = vec![mix; FRAMES];
            Metronome::default().render(
                Some(transport(0, 0.0)),
                SessionFrame::new(START),
                node.level,
                node.shape,
                &mut left,
                &mut right,
            );
            left
        };
        let loud = render(0.5);
        let soft = render(0.25);

        // WHY: Both mixes carry the same clicks, so their difference over the
        // difference of the mixes is the gain the duck leaves the mix.
        let gains: Vec<f32> = loud
            .iter()
            .zip(&soft)
            .map(|(loud, soft)| (loud - soft) / 0.25)
            .collect();
        let beat_two = usize::try_from(2 * 22_050 - START).expect("beat 2 inside the block");
        assert!(
            gains.get(beat_two).is_some_and(|gain| *gain < 1.0),
            "beat 2 clicks while the duck of beat 1 is still releasing"
        );
        let max_step = PI / (2.0 * 88.0);
        let jump = gains
            .windows(2)
            .enumerate()
            .find(|(_, pair)| (pair[1] - pair[0]).abs() > max_step + TOLERANCE);
        assert!(
            jump.is_none(),
            "the duck moves no faster than its rise: {jump:?}"
        );
    }

    #[kithara::test]
    fn a_metronome_level_sits_above_zero_and_at_most_one() {
        let refused = |level| {
            matches!(
                MetronomeConfig::builder().level(level).build().validated(),
                Err(PlayError::InvalidParameter { name, .. }) if name == "metronome_level"
            )
        };
        assert!(refused(f32::NAN), "NaN");
        assert!(refused(0.0), "zero");
        assert!(refused(1.01), "over the ceiling");
        assert!(
            MetronomeConfig::builder()
                .level(0.01)
                .build()
                .validated()
                .is_ok(),
            "a quiet click"
        );
        assert!(
            MetronomeConfig::builder()
                .level(1.0)
                .build()
                .validated()
                .is_ok(),
            "a click at the ceiling"
        );
    }

    #[kithara::test]
    fn a_metronome_duck_sits_between_zero_and_one() {
        let duck = |depth| {
            MetronomeConfig::builder()
                .level(0.5)
                .duck(depth)
                .build()
                .validated()
        };
        let refused = |depth| {
            matches!(
                duck(depth),
                Err(PlayError::InvalidParameter { name, .. }) if name == "metronome_duck"
            )
        };
        assert!(refused(f32::NAN), "NaN");
        assert!(refused(-0.01), "a duck that lifts the mix");
        assert!(refused(1.01), "deeper than muting the mix");
        assert!(duck(0.0).is_ok(), "no duck");
        assert!(duck(0.49).is_ok(), "shallower than the level");
        assert!(duck(1.0).is_ok(), "a full duck");
    }

    #[kithara::test]
    fn a_metronome_click_shape_keeps_its_bounds() {
        let refused = |config: Result<MetronomeConfig, PlayError>, parameter: &str| {
            matches!(
                config,
                Err(PlayError::InvalidParameter { name, .. }) if name == parameter
            )
        };
        let decay = |decay| MetronomeConfig::builder().decay(decay).build().validated();
        let hold = |hold| MetronomeConfig::builder().hold(hold).build().validated();
        let release = |release| {
            MetronomeConfig::builder()
                .release(release)
                .build()
                .validated()
        };
        let millis = Duration::from_millis;
        assert!(refused(decay(millis(7)), "metronome_decay"), "decay 7 ms");
        assert!(decay(millis(8)).is_ok(), "decay 8 ms");
        assert!(decay(millis(50)).is_ok(), "decay 50 ms");
        assert!(refused(decay(millis(51)), "metronome_decay"), "decay 51 ms");
        assert!(hold(Duration::ZERO).is_ok(), "no hold");
        assert!(hold(millis(1_000)).is_ok(), "hold 1 s");
        assert!(
            refused(hold(millis(1_001)), "metronome_hold"),
            "hold 1.001 s"
        );
        assert!(
            refused(release(millis(7)), "metronome_release"),
            "release 7 ms"
        );
        assert!(release(millis(8)).is_ok(), "release 8 ms");
        assert!(release(millis(1_000)).is_ok(), "release 1 s");
        assert!(
            refused(release(millis(1_001)), "metronome_release"),
            "release 1.001 s"
        );
    }

    #[kithara::test]
    fn a_metronome_level_change_passes_only_a_level_in_its_bounds() {
        let base = config(0.5, 0.5);
        for level in [f32::NAN, 0.0, 1.01] {
            let refused = MetronomeConfig::check(MetronomeConfigChange::Level(level));
            assert!(
                matches!(
                    &refused,
                    Err(PlayError::InvalidParameter { name, .. }) if name == "metronome_level"
                ),
                "a level of {level} is refused: {refused:?}"
            );
        }
        for level in [0.25, 0.5, 1.0] {
            let mut updated = base;
            let change = MetronomeConfig::check(MetronomeConfigChange::Level(level))
                .expect("a level in its bounds");
            updated.apply_change(change);
            assert_eq!(
                updated,
                MetronomeConfig { level, ..base },
                "a new level keeps the duck and the click's shape"
            );
        }
    }
}
