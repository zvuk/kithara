#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    audio::mock::TestPcmReader,
    host::{
        HostConfig, HostSettings, HostSettingsControl, MetronomeConfig, MetronomeConfigControl,
        Tap, api::SESSION_BEATS_PER_BAR,
    },
    play::Tempo,
    signal::AudioSpec,
    warp::BeatGridSnapshot,
};
use kithara_integration_tests::{
    audio_artifact::{AudioArtifactTap, artifact_label},
    bufpool_ext::{TestPools, pools},
    offline::{OfflineHostHarness, OfflinePlayer, OfflinePlayerOptions, resource_from_reader},
};

use super::{
    metronome::{assert_click_levels, beat_frame, capacity, clicks},
    mix_tap::{play_resource, render_blocks},
};

mod consts {
    pub(super) const SAMPLE_RATE: u32 = 44_100;
    pub(super) const BLOCK_FRAMES: u32 = 512;
    pub(super) const CHANNELS: u16 = 2;
    /// The Host eases a tempo change in: the tempo approaches the new one as
    /// `e^(-t/τ)` with this time constant, in seconds.
    pub(super) const TEMPO_SMOOTH_SECONDS: f64 = 0.005;
    /// The Host tempo a ride starts from: the tempo a Host starts at.
    pub(super) const RIDE_FROM_BPM: u32 = 120;
    /// The Host tempo a ride ends at, one BPM more on each change.
    pub(super) const RIDE_TO_BPM: u32 = 145;
    /// Blocks each ride tempo holds: under two beats, so the changes land on
    /// ever different phases of a beat.
    pub(super) const STEP_BLOCKS: u64 = 77;
    /// Blocks the last ride tempo holds: over eight beats.
    pub(super) const LAST_BLOCKS: u64 = 308;
    /// The Host places a beat on the frame nearest its exact time: half a
    /// frame, with slack for the float noise of two computations.
    pub(super) const NEAREST_FRAME: f64 = 0.5 + 1e-9;
    /// A beat this close to a tempo commit could be placed on either side of
    /// it, and this close to the end of a take could click past it: the ride
    /// keeps every beat further away.
    pub(super) const BOUNDARY_FRAMES: f64 = 2.0;
    /// Halvings that pin an exact beat frame far below float resolution.
    pub(super) const BISECTION_STEPS: usize = 128;
    /// A click's first sounding frame follows its beat frame: the click
    /// starts from silence.
    pub(super) const SILENT_FOOT: u64 = 1;
    /// The default metronome level: a downbeat click at the limiter ceiling.
    pub(super) const FULL_LEVEL: f32 = 1.0;
    /// Frames a beat lasts at the tempo a Host starts at, 120 BPM, and
    /// [`SAMPLE_RATE`].
    pub(super) const OVERLAY_PERIOD: u64 = 22_050;
    /// Beats the recorded metronome clicks: four bars.
    pub(super) const OVERLAY_BEATS: u64 = 16;
    /// Metronome level of every overlay take: the recorded click passes the
    /// limiter untouched, and with the live one on top it meets the ceiling.
    pub(super) const OVERLAY_LEVEL: f32 = 0.5;
    /// No duck: the live click adds to the mix and leaves it whole.
    pub(super) const NO_DUCK: f32 = 0.0;
}

/// One stretch of the Host tempo as the math has it. From `frame` on, the
/// tempo eases from `from` to `to` beats per second as
/// `to + (from - to)·e^(-t/τ)` and the beat grows from `beat` by its
/// integral.
#[derive(Clone, Copy, Debug)]
struct Stretch {
    frame: f64,
    beat: f64,
    from: f64,
    to: f64,
}

impl Stretch {
    /// The first tempo, set on `frame` with no tempo before it: beat 0.
    fn first(frame: u64, bpm: u32) -> Self {
        let tempo = beats_per_second(bpm);
        Self {
            frame: exact(frame),
            beat: 0.0,
            from: tempo,
            to: tempo,
        }
    }

    /// The stretch a change to `bpm` committed on `frame` starts: it goes on
    /// from the beat and the tempo this one reached there.
    fn then(self, frame: u64, bpm: u32) -> Self {
        let frame = exact(frame);
        Self {
            frame,
            beat: self.beat_at(frame),
            from: self.tempo_at(frame),
            to: beats_per_second(bpm),
        }
    }

    fn seconds(self, frame: f64) -> f64 {
        (frame - self.frame) / f64::from(consts::SAMPLE_RATE)
    }

    fn tempo_at(self, frame: f64) -> f64 {
        let eased = (-self.seconds(frame) / consts::TEMPO_SMOOTH_SECONDS).exp();
        self.to + (self.from - self.to) * eased
    }

    fn beat_at(self, frame: f64) -> f64 {
        let seconds = self.seconds(frame);
        let eased =
            -consts::TEMPO_SMOOTH_SECONDS * (-seconds / consts::TEMPO_SMOOTH_SECONDS).exp_m1();
        self.beat + self.to * seconds + (self.from - self.to) * eased
    }

    /// The exact, fractional frame this stretch reaches `beat` on. The beat
    /// grows at least as fast as the slower of the two tempos, which bounds
    /// the search.
    fn frame_of(self, beat: f64) -> f64 {
        let slowest = self.from.min(self.to);
        let mut lower = self.frame;
        let mut upper =
            self.frame + (beat - self.beat) / slowest * f64::from(consts::SAMPLE_RATE) + 1.0;
        for _ in 0..consts::BISECTION_STEPS {
            let middle = lower.midpoint(upper);
            if self.beat_at(middle) < beat {
                lower = middle;
            } else {
                upper = middle;
            }
        }
        lower.midpoint(upper)
    }
}

/// A whole beat the math places: its Host ordinal, its exact frame and the
/// stretch whose tempo places it.
#[derive(Clone, Copy, Debug)]
struct ComputedBeat {
    ordinal: u32,
    frame: f64,
    stretch: usize,
}

impl ComputedBeat {
    fn downbeat(self) -> bool {
        self.ordinal
            .is_multiple_of(u32::from(SESSION_BEATS_PER_BAR.get()))
    }
}

fn beats_per_second(bpm: u32) -> f64 {
    f64::from(bpm) / 60.0
}

fn exact(frame: u64) -> f64 {
    f64::from(u32::try_from(frame).expect("a take fits u32 frames"))
}

/// Every whole beat the math places before `end`, stretch by stretch.
fn computed_beats(stretches: &[Stretch], end: u64) -> Vec<ComputedBeat> {
    let end = exact(end);
    let mut beats = Vec::new();
    let mut ordinal = 0_u32;
    for (index, stretch) in stretches.iter().enumerate() {
        let until = stretches.get(index + 1).map_or(end, |next| next.frame);
        loop {
            let frame = stretch.frame_of(f64::from(ordinal));
            if frame >= until {
                break;
            }
            beats.push(ComputedBeat {
                ordinal,
                frame,
                stretch: index,
            });
            ordinal += 1;
        }
    }
    let boundaries: Vec<f64> = stretches
        .iter()
        .skip(1)
        .map(|stretch| stretch.frame)
        .chain(std::iter::once(end))
        .collect();
    let crowded = beats.iter().find(|beat| {
        boundaries
            .iter()
            .any(|boundary| (beat.frame - boundary).abs() < consts::BOUNDARY_FRAMES)
    });
    assert!(
        crowded.is_none(),
        "every beat sits clear of a tempo commit and of the take's end: {crowded:?}"
    );
    beats
}

/// One tempo of a ride: its BPM, the frame it commits on and the grid the
/// Host published for it.
struct RideStep {
    bpm: u32,
    at: u64,
    grid: BeatGridSnapshot,
}

/// A tempo ride the Host rendered with no deck: its output tap from frame
/// `start`, every tempo it was given and the frame the ride ended on.
struct HostRide {
    output: Vec<f32>,
    start: u64,
    end: u64,
    steps: Vec<RideStep>,
}

impl HostRide {
    /// The stretches the math places the ride's beats with: the Host runs at
    /// the first tempo from the first frame it renders, and each change
    /// commits on its scheduled frame, where the next block starts.
    fn stretches(&self) -> Vec<Stretch> {
        let mut stretches: Vec<Stretch> = Vec::with_capacity(self.steps.len());
        for step in &self.steps {
            let stretch = stretches.last().map_or_else(
                || Stretch::first(self.start, step.bpm),
                |last| last.then(step.at, step.bpm),
            );
            stretches.push(stretch);
        }
        stretches
    }
}

/// Rides an offline Host with its metronome on and no deck from
/// [`consts::RIDE_FROM_BPM`] to [`consts::RIDE_TO_BPM`], one BPM a step.
async fn host_ride() -> HostRide {
    let block = u64::from(consts::BLOCK_FRAMES);
    let steps = u64::from(consts::RIDE_TO_BPM - consts::RIDE_FROM_BPM);
    let frames = block * (1 + steps * consts::STEP_BLOCKS + consts::LAST_BLOCKS);
    let config = HostConfig::offline(pools())
        .settings(
            HostSettings::builder()
                .sample_rate(NonZeroU32::new(consts::SAMPLE_RATE).expect("test sample rate"))
                .build(),
        )
        .max_block_frames(NonZeroU32::new(consts::BLOCK_FRAMES).expect("test block size"))
        .build();
    let host = OfflineHostHarness::new(config)
        .await
        .expect("offline Host without a deck");
    let mut tap = host
        .attach_tap(Tap::Output, capacity(frames))
        .await
        .expect("output tap");
    host.with(|host| host.metronome().set_enabled(true))
        .await
        .expect("metronome on");
    let start = host.position();
    host.render_forward(block).await;

    let mut ride = Vec::new();
    let mut at = host.position();
    for bpm in consts::RIDE_FROM_BPM..=consts::RIDE_TO_BPM {
        let tempo = Tempo::new(f64::from(bpm)).expect("ride tempo");
        if host.with(|host| host.tempo()).await != tempo {
            assert_eq!(
                host.position() + block,
                at,
                "one delivery before the change"
            );
            let revision = host.session_grid().await.revision();
            host.with(move |host| host.set_tempo(tempo))
                .await
                .expect("Host tempo");
            host.render_forward(2 * block).await;
            assert_ne!(
                host.session_grid().await.revision(),
                revision,
                "the Host publishes the {bpm} BPM grid once the block it commits on renders"
            );
        }
        ride.push(RideStep {
            bpm,
            at,
            grid: host.session_grid().await,
        });
        let hold = if bpm == consts::RIDE_TO_BPM {
            consts::LAST_BLOCKS
        } else {
            consts::STEP_BLOCKS
        };
        let until = at + hold * block;
        host.render_forward(until - block - host.position()).await;
        at = until;
    }
    host.render_forward(block).await;
    let end = host.position();
    host.close().await;

    assert_eq!(tap.drops(), 0, "the tap keeps every frame");
    HostRide {
        output: tap.drain(),
        start,
        end,
        steps: ride,
    }
}

#[kithara::test(tokio)]
async fn the_host_grid_places_every_beat_on_the_frame_nearest_its_computed_time() {
    let ride = host_ride().await;
    let beats = computed_beats(&ride.stretches(), ride.end);

    assert_eq!(
        beat_frame(&ride.steps[0].grid, 0),
        ride.start,
        "the Host starts with beat 0 on the first frame it renders"
    );
    assert!(
        beats.len() > ride.steps.len(),
        "the ride spans more beats than tempo changes: {}",
        beats.len()
    );
    // WHY: Each published grid covers the stretch from its own commit on;
    // a beat is read from the grid of the stretch the math places it in.
    let misplaced: Vec<(u32, u64, f64)> = beats
        .iter()
        .filter_map(|beat| {
            let host = beat_frame(&ride.steps[beat.stretch].grid, i64::from(beat.ordinal));
            ((exact(host) - beat.frame).abs() > consts::NEAREST_FRAME).then_some((
                beat.ordinal,
                host,
                beat.frame,
            ))
        })
        .collect();
    assert!(
        misplaced.is_empty(),
        "the Host grid places every beat on the frame nearest its computed time \
         (ordinal, Host frame, computed frame): {misplaced:?}"
    );
}

#[kithara::test(tokio)]
async fn the_metronome_clicks_on_the_frame_nearest_every_computed_beat() {
    let ride = host_ride().await;
    let beats = computed_beats(&ride.stretches(), ride.end);

    if let Some(mut artifact) =
        AudioArtifactTap::from_env(&artifact_label(), consts::SAMPLE_RATE, consts::CHANNELS)
            .expect("listening artifact")
    {
        artifact.push(&ride.output);
    }
    let heard = clicks(&ride.output);
    assert_eq!(
        heard.len(),
        beats.len(),
        "one click sounds for every computed beat, and no other"
    );
    let off: Vec<(u32, u64, f64)> = heard
        .iter()
        .zip(&beats)
        .filter_map(|(click, beat)| {
            let placed = click.frame + ride.start - consts::SILENT_FOOT;
            ((exact(placed) - beat.frame).abs() > consts::NEAREST_FRAME).then_some((
                beat.ordinal,
                placed,
                beat.frame,
            ))
        })
        .collect();
    assert!(
        off.is_empty(),
        "every click rises from the frame nearest its computed beat \
         (ordinal, click beat frame, computed frame): {off:?}"
    );
    let accents: Vec<(u64, bool)> = heard
        .iter()
        .zip(&beats)
        .map(|(click, beat)| (click.frame, beat.downbeat()))
        .collect();
    assert_click_levels(&heard, &accents, consts::FULL_LEVEL);
}

/// The Host of every overlay take: the metronome at
/// [`consts::OVERLAY_LEVEL`] with no duck.
fn overlay_session() -> HostConfig<TestPools> {
    HostConfig::offline(pools())
        .max_block_frames(NonZeroU32::new(consts::BLOCK_FRAMES).expect("test block size"))
        .settings(
            HostSettings::builder()
                .sample_rate(NonZeroU32::new(consts::SAMPLE_RATE).expect("test sample rate"))
                .metronome(
                    MetronomeConfig::builder()
                        .level(consts::OVERLAY_LEVEL)
                        .duck(consts::NO_DUCK)
                        .build(),
                )
                .build(),
        )
        .build()
}

/// The Host metronome with no deck under it, recorded from the first frame
/// the Host renders: [`consts::OVERLAY_BEATS`] beats, beat 0 on frame 0.
async fn recorded_metronome() -> Vec<f32> {
    let frames = consts::OVERLAY_BEATS * consts::OVERLAY_PERIOD;
    let host = OfflineHostHarness::new(overlay_session())
        .await
        .expect("offline Host without a deck");
    let mut tap = host
        .attach_tap(Tap::Output, capacity(frames))
        .await
        .expect("output tap");
    host.with(|host| host.metronome().set_enabled(true))
        .await
        .expect("metronome on");
    host.render_forward(frames).await;
    host.close().await;

    assert_eq!(tap.drops(), 0, "the tap keeps every frame");
    tap.drain()
}

/// A deck playing `recording` after `pad` frames of silence, settled from
/// its first frame and never short of decoded audio.
async fn overlay_deck(recording: &[f32], pad: u64) -> OfflinePlayer {
    // WHY: The PCM reader plays one sample a frame on every channel; the
    // metronome clicks both channels alike.
    let pad = usize::try_from(pad).expect("pad frames");
    let samples: Vec<f32> = std::iter::repeat_n(0.0, pad)
        .chain(
            recording
                .iter()
                .step_by(usize::from(consts::CHANNELS))
                .copied(),
        )
        .collect();
    let rate = NonZeroU32::new(consts::SAMPLE_RATE).expect("test sample rate");
    let options = OfflinePlayerOptions::builder()
        .crossfade_duration(0.0)
        .block_on_underrun(true)
        .build();
    play_resource(
        OfflinePlayer::with_options(options, overlay_session()).await,
        move || {
            resource_from_reader(TestPcmReader::with_samples(
                AudioSpec::new(consts::CHANNELS, rate),
                samples,
            ))
        },
    )
    .await
}

/// The session frame an [`overlay_deck`] padded by `pad` plays frame 0 of
/// `recording` on, found with the metronome off: the deck's first sound less
/// the frames the recording is silent before its first click.
async fn recording_start(recording: &[f32], pad: u64) -> u64 {
    let lead = clicks(recording)
        .first()
        .expect("the recording clicks")
        .frame;
    let harness = overlay_deck(recording, pad).await;
    let host = harness.host();
    let horizon = pad + consts::OVERLAY_PERIOD;
    let mut master = host
        .attach_tap(
            Tap::Master,
            capacity(horizon + u64::from(consts::BLOCK_FRAMES)),
        )
        .await
        .expect("master tap");
    let start = host.position();
    while host.position() < start + horizon {
        render_blocks(&harness, 1).await;
    }
    harness.close().await;

    assert_eq!(master.drops(), 0, "the tap keeps every frame");
    let first = clicks(&master.drain())
        .first()
        .expect("the deck sounds the recording")
        .frame;
    start + first - lead
}

/// The first frame `heard` differs from `expected` on, counted from the
/// first frame of `heard`, with both samples there.
fn first_difference(heard: &[f32], expected: &[f32]) -> Option<(usize, f32, f32)> {
    heard
        .iter()
        .zip(expected)
        .position(|(heard, expected)| heard != expected)
        .map(|sample| {
            (
                sample / usize::from(consts::CHANNELS),
                heard[sample],
                expected[sample],
            )
        })
}

#[kithara::test(tokio)]
async fn the_live_metronome_lands_on_its_own_recording_with_no_flam() {
    let recording = recorded_metronome().await;
    assert_eq!(
        clicks(&recording).len(),
        usize::try_from(consts::OVERLAY_BEATS).expect("beats"),
        "the recording clicks every beat"
    );
    // WHY: The live Host runs at the recording's tempo from its first frame,
    // so its bars open on whole multiples of a bar. Padded to start on one,
    // the recording's beat 0 meets a live downbeat on another phase of a
    // render block than it was recorded on. The probe pads a beat, so the
    // deck's first sound renders after the taps attach.
    let bar = consts::OVERLAY_PERIOD * u64::from(SESSION_BEATS_PER_BAR.get());
    let probe = recording_start(&recording, consts::OVERLAY_PERIOD).await;
    let pad = consts::OVERLAY_PERIOD + bar - probe % bar;
    let at = recording_start(&recording, pad).await;
    assert_eq!(at % bar, 0, "the recording starts on a Host bar");

    let harness = overlay_deck(&recording, pad).await;
    let host = harness.host();
    let frames = consts::OVERLAY_BEATS * consts::OVERLAY_PERIOD;
    let start = host.position();
    let held = capacity(at + frames + u64::from(consts::BLOCK_FRAMES) - start);
    let mut master = host
        .attach_tap(Tap::Master, held)
        .await
        .expect("master tap");
    let mut output = host
        .attach_tap(Tap::Output, held)
        .await
        .expect("output tap");
    // WHY: A metronome switched on clicks from the next beat: switched on in
    // the block before the recording, it clicks from the recording's beat 0.
    // Admission precedes that block by one delivery.
    while host.position() + 2 * u64::from(consts::BLOCK_FRAMES) < at {
        render_blocks(&harness, 1).await;
    }
    host.with(|host| host.metronome().set_enabled(true))
        .await
        .expect("metronome on");
    while host.position() < at + frames {
        render_blocks(&harness, 1).await;
    }
    harness.close().await;

    assert_eq!(
        (output.drops(), master.drops()),
        (0, 0),
        "the taps keep every frame"
    );
    let mut placed = vec![0.0; capacity(at - start)];
    placed.extend_from_slice(&recording);
    let master = master.drain();
    let output = output.drain();
    assert!(
        master.len() >= placed.len() && output.len() >= placed.len(),
        "the taps hold the whole recording"
    );
    if let Some(mut artifact) =
        AudioArtifactTap::from_env(&artifact_label(), consts::SAMPLE_RATE, consts::CHANNELS)
            .expect("listening artifact")
    {
        artifact.push(&output[capacity(at - start)..placed.len()]);
    }
    assert_eq!(
        first_difference(&master, &placed),
        None,
        "the deck plays the recorded metronome from frame {at} on \
         (frame from {start}, master, recording)"
    );
    // WHY: With no duck the output is the mix plus the live click. The live
    // click lands on the recorded one sample for sample exactly when the
    // output is twice the mix: any offset sounds as a flam.
    let doubled: Vec<f32> = placed.iter().map(|sample| sample * 2.0).collect();
    assert_eq!(
        first_difference(&output, &doubled),
        None,
        "the live metronome clicks on every recorded click, sample for sample \
         (frame from {start}, output, twice the recording)"
    );
}
