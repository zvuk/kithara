#![forbid(unsafe_code)]

//! The feedback delay a deck FX unit holds: echo positions and amplitudes in
//! any block partition, the cross-read of a time change, the place of the
//! feedback filter in the loop, the reported tail and the refusals.

use std::{
    f64::consts::{FRAC_1_SQRT_2, PI},
    num::{NonZeroU32, NonZeroUsize},
};

use kithara_effects::{
    DeckEffect, FxParamChange,
    delay::{
        DelayParam, DelayTime, Feedback, FeedbackDelay, FeedbackDelayConfig, FeedbackHighPass,
    },
};
use kithara_test_utils::{bufpool::pools, kithara};
use num_traits::ToPrimitive;

const RATE: u32 = 44_100;
/// Frames a time change crosses over in: the declared 20 ms at [`RATE`].
const RAMP: u32 = 882;
/// The -120 dBFS floor under which a reported tail stays.
const FLOOR: f64 = 1e-6;
/// How far the effect may sit from the `f64` model of its loop.
const MODEL: f64 = 1e-6;
const REGULAR: [usize; 1] = [64];
const IRREGULAR: [usize; 6] = [37, 64, 13, 50, 1, 63];

#[derive(Clone, Copy)]
struct Settings {
    capacity: usize,
    time: u32,
    feedback: f32,
    high_pass: f32,
}

fn config(settings: Settings) -> FeedbackDelayConfig {
    FeedbackDelayConfig::builder()
        .sample_rate(NonZeroU32::new(RATE).expect("a nonzero rate"))
        .capacity(NonZeroUsize::new(settings.capacity).expect("a nonzero capacity"))
        .time(DelayTime::checked(f64::from(settings.time)).expect("a representable delay"))
        .feedback(Feedback::checked(settings.feedback).expect("a feedback below unity"))
        .high_pass(FeedbackHighPass::checked(settings.high_pass).expect("a cutoff in range"))
        .build()
}

fn delay(settings: Settings) -> FeedbackDelay {
    FeedbackDelay::new(config(settings), &pools()).expect("the delay fits the test pools")
}

fn time(frames: f64) -> FxParamChange {
    FxParamChange::Delay(DelayParam::Time(
        DelayTime::checked(frames).expect("a representable delay"),
    ))
}

fn frames(count: u32) -> usize {
    usize::try_from(count).expect("a frame count fits usize")
}

fn index(frame: usize) -> f64 {
    f64::from(u32::try_from(frame).expect("a test frame fits u32"))
}

fn sample(value: f64) -> f32 {
    value.to_f32().expect("a test sample fits f32")
}

/// The wet signal `effect` makes of `input`, called in blocks of the sizes
/// `blocks` cycles through, each of `changes` set on its frame.
fn run(
    effect: &mut FeedbackDelay,
    input: &[Vec<f32>; 2],
    blocks: &[usize],
    changes: &[(usize, FxParamChange)],
) -> [Vec<f32>; 2] {
    let length = input[0].len();
    let mut wet = [vec![0.0; length], vec![0.0; length]];
    let mut changes = changes.iter().peekable();
    let mut start = 0;
    for block in blocks.iter().cycle() {
        if start == length {
            break;
        }
        while let Some((_, change)) = changes.next_if(|(at, _)| *at == start) {
            assert!(effect.accepts(change), "the delay takes {change:?}");
            effect.set(*change);
        }
        let due = changes.peek().map_or(length, |(at, _)| *at);
        let end = (start + block).min(due);
        let [left, right] = &mut wet;
        effect.process(
            [&input[0][start..end], &input[1][start..end]],
            [&mut left[start..end], &mut right[start..end]],
        );
        start = end;
    }
    wet
}

fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|sample| sample.to_bits()).collect()
}

fn assert_near(actual: &[f32], expected: &[f64], what: &str) {
    assert_eq!(actual.len(), expected.len(), "{what}: length");
    let worst = actual
        .iter()
        .zip(expected)
        .map(|(actual, expected)| (f64::from(*actual) - expected).abs())
        .enumerate()
        .max_by(|(_, left), (_, right)| left.total_cmp(right));
    assert!(
        worst.is_none_or(|(_, error)| error <= MODEL),
        "{what}: worst frame and error {worst:?}"
    );
}

/// The cookbook high-pass at `cutoff` with Butterworth Q, its coefficients
/// rounded to `f32` as a filter running in `f32` holds them:
/// `[b0, b1, b2, a1, a2]`.
fn high_pass(cutoff: f32) -> [f64; 5] {
    let omega = 2.0 * PI * f64::from(cutoff) / f64::from(RATE);
    let (sin, cos) = omega.sin_cos();
    let alpha = sin / (2.0 * FRAC_1_SQRT_2);
    let scale = 1.0 / (1.0 + alpha);
    [
        (1.0 + cos) / 2.0,
        -(1.0 + cos),
        (1.0 + cos) / 2.0,
        -2.0 * cos,
        1.0 - alpha,
    ]
    .map(|coefficient| f64::from(sample(coefficient * scale)))
}

/// A biquad in `f64`, direct form I.
struct Section {
    coefficients: [f64; 5],
    state: [f64; 4],
}

impl Section {
    fn new(cutoff: f32) -> Self {
        Self {
            coefficients: high_pass(cutoff),
            state: [0.0; 4],
        }
    }

    fn step(&mut self, input: f64) -> f64 {
        let [b0, b1, b2, a1, a2] = self.coefficients;
        let [x1, x2, y1, y2] = self.state;
        let output = b0 * input + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        self.state = [input, x1, output, y1];
        output
    }
}

/// The delay loop in scalar `f64`: the wet signal reads the history at the
/// delay, crossing linearly from the previous delay over [`RAMP`] frames, and
/// the history takes the input plus the wet signal high-passed once and
/// scaled by the feedback.
struct Model {
    wet: Vec<f64>,
    history: Vec<f64>,
    filter: Section,
    feedback: f64,
    time: usize,
    ramp: Option<(usize, u32)>,
}

impl Model {
    /// The model having played `input`, its delay moving to each of
    /// `changes` on its frame.
    fn play(settings: Settings, input: &[f32], changes: &[(usize, u32)]) -> Self {
        let mut model = Self {
            wet: Vec::with_capacity(input.len()),
            history: Vec::with_capacity(input.len()),
            filter: Section::new(settings.high_pass),
            feedback: f64::from(settings.feedback),
            time: frames(settings.time),
            ramp: None,
        };
        for (frame, sample) in input.iter().enumerate() {
            for (_, time) in changes.iter().filter(|(at, _)| *at == frame) {
                model.ramp = Some((model.time, 0));
                model.time = frames(*time);
            }
            model.step(*sample);
        }
        model
    }

    fn read(&self, delay: usize) -> f64 {
        self.history
            .len()
            .checked_sub(delay)
            .map_or(0.0, |frame| self.history[frame])
    }

    fn step(&mut self, input: f32) {
        let new = self.read(self.time);
        let wet = self.ramp.map_or(new, |(from, done)| {
            let old = self.read(from);
            old + f64::from(done + 1) / f64::from(RAMP) * (new - old)
        });
        let filtered = self.filter.step(wet);
        self.history
            .push(f64::from(input) + self.feedback * filtered);
        self.wet.push(wet);
        self.ramp = self
            .ramp
            .and_then(|(from, done)| (done + 1 < RAMP).then_some((from, done + 1)));
    }
}

/// A unit impulse on the left and a smaller inverted one three frames later
/// on the right, then silence.
fn impulses(length: usize) -> [Vec<f32>; 2] {
    let mut input = [vec![0.0; length], vec![0.0; length]];
    input[0][0] = 1.0;
    input[1][3] = -0.5;
    input
}

fn largest_step(samples: impl Iterator<Item = f64> + Clone) -> f64 {
    samples
        .clone()
        .zip(samples.skip(1))
        .map(|(previous, next)| (next - previous).abs())
        .fold(0.0, f64::max)
}

#[kithara::test]
fn feedback_delay_echoes_land_on_their_frames_in_any_partition() {
    const TIME: u32 = 1000;
    let settings = Settings {
        capacity: 1024,
        time: TIME,
        feedback: 0.5,
        high_pass: f32::from(FeedbackHighPass::MIN),
    };
    let time = frames(TIME);
    let input = impulses(8 * time);
    let regular = run(&mut delay(settings), &input, &REGULAR, &[]);
    let irregular = run(&mut delay(settings), &input, &IRREGULAR, &[]);
    assert_eq!(
        regular.each_ref().map(|plane| bits(plane)),
        irregular.each_ref().map(|plane| bits(plane)),
        "the echoes render the same in any block partition"
    );
    let [left, right] = &regular;
    assert!(
        left[..time].iter().all(|sample| *sample == 0.0),
        "nothing sounds before the first echo"
    );
    assert_eq!(left[time], 1.0, "the first echo is the impulse itself");
    assert!(
        left[time + 1..2 * time].iter().all(|sample| *sample == 0.0),
        "the first echo lasts one frame"
    );
    assert_ne!(left[2 * time], 0.0, "the second echo lands one delay later");
    assert_eq!(right[time + 3], -0.5, "each channel echoes its own input");
}

#[kithara::test]
fn feedback_delay_echo_amplitudes_follow_the_feedback_law() {
    const TIME: u32 = 40;
    let settings = Settings {
        capacity: 64,
        time: TIME,
        feedback: 0.5,
        high_pass: f32::from(FeedbackHighPass::MAX),
    };
    let input = impulses(12 * frames(TIME));
    for blocks in [&REGULAR[..], &IRREGULAR[..]] {
        let wet = run(&mut delay(settings), &input, blocks, &[]);
        for (channel, (wet, input)) in wet.iter().zip(&input).enumerate() {
            assert_near(
                wet,
                &Model::play(settings, input, &[]).wet,
                &format!("channel {channel} in blocks {blocks:?}"),
            );
        }
    }
}

#[kithara::test]
fn feedback_delay_time_change_crosses_over_within_its_ramp() {
    const FROM: u32 = 1000;
    const TO: u32 = 1007;
    const CHANGE: usize = 6000 + 37;
    let settings = Settings {
        capacity: 1024,
        time: FROM,
        feedback: 0.5,
        high_pass: f32::from(FeedbackHighPass::MAX),
    };
    let length = CHANGE + 2 * frames(RAMP) + 1000;
    let input: [Vec<f32>; 2] = [3000.0, 2000.0].map(|frequency| {
        (0..length)
            .map(|frame| {
                sample(0.5 * (2.0 * PI * frequency * index(frame) / f64::from(RATE)).sin())
            })
            .collect()
    });
    let steady = run(&mut delay(settings), &input, &REGULAR, &[]);
    let changed = run(
        &mut delay(settings),
        &input,
        &REGULAR,
        &[(CHANGE, time(f64::from(TO)))],
    );
    for (channel, ((changed, steady), input)) in changed.iter().zip(&steady).zip(&input).enumerate()
    {
        assert_eq!(
            bits(&changed[..CHANGE]),
            bits(&steady[..CHANGE]),
            "channel {channel}: nothing moves before the change's frame"
        );
        assert_ne!(
            changed[CHANGE], steady[CHANGE],
            "channel {channel}: the cross-read starts on the change's frame"
        );
        let model = Model::play(settings, input, &[(CHANGE, TO)]);
        assert_near(
            changed,
            &model.wet,
            &format!("channel {channel}: the cross-read completes within its ramp"),
        );
        let step = largest_step(model.history.iter().copied());
        let peak = model
            .history
            .iter()
            .map(|value| value.abs())
            .fold(0.0, f64::max);
        let bound = step + 2.0 * peak / f64::from(RAMP) + 2.0 * MODEL;
        let jump = largest_step(
            changed[CHANGE - 1..=CHANGE + frames(RAMP)]
                .iter()
                .map(|sample| f64::from(*sample)),
        );
        assert!(
            jump <= bound,
            "channel {channel}: the cross-read steps {jump}, above the declared {bound}"
        );
    }
}

#[kithara::test]
fn feedback_delay_filters_each_repeat_once_more() {
    const TIME: u32 = 512;
    const PULSE: usize = 64;
    const FEEDBACK: f32 = 0.8;
    let settings = Settings {
        capacity: frames(TIME),
        time: TIME,
        feedback: FEEDBACK,
        high_pass: f32::from(FeedbackHighPass::MAX),
    };
    let time = frames(TIME);
    let mut pulse = vec![0.0; 6 * time];
    for (frame, sample) in pulse[..PULSE].iter_mut().enumerate() {
        *sample = self::sample(0.5 - 0.5 * (2.0 * PI * index(frame) / index(PULSE)).cos());
    }
    let input = [pulse.clone(), vec![0.0; pulse.len()]];
    let [wet, silent] = run(&mut delay(settings), &input, &REGULAR, &[]);
    assert!(
        silent.iter().all(|sample| *sample == 0.0),
        "a silent channel stays silent"
    );
    assert_eq!(
        bits(&wet[time..2 * time]),
        bits(&pulse[..time]),
        "the first echo is the input unfiltered: the filter is not on the wet path"
    );
    for repeat in 1..5 {
        let mut filter = Section::new(settings.high_pass);
        let expected: Vec<f64> = wet[repeat * time..(repeat + 1) * time]
            .iter()
            .map(|sample| f64::from(FEEDBACK) * filter.step(f64::from(*sample)))
            .collect();
        assert_near(
            &wet[(repeat + 1) * time..(repeat + 2) * time],
            &expected,
            &format!("echo {} is echo {repeat} filtered once more", repeat + 1),
        );
    }
}

#[kithara::test]
fn feedback_delay_reports_its_tail_spent_within_the_bound() {
    const TIME: u32 = 256;
    const CAPACITY: usize = 512;
    const FEEDBACK: f32 = 0.5;
    let settings = Settings {
        capacity: CAPACITY,
        time: TIME,
        feedback: FEEDBACK,
        high_pass: f32::from(FeedbackHighPass::MIN),
    };
    let passes = (FLOOR.ln() / f64::from(FEEDBACK).ln()).ceil();
    let bound = CAPACITY + frames(TIME) * passes.to_usize().expect("a whole count of passes");
    let mut effect = delay(settings);
    assert!(effect.tail_exhausted(), "a prepared delay holds silence");
    let mut wet = [0.0_f32];
    let mut right = [0.0_f32];
    let mut reported = None;
    for frame in 0..bound + 2 * CAPACITY {
        let input = [if frame == 0 { 1.0 } else { 0.0 }];
        effect.process([&input, &[0.0]], [&mut wet, &mut right]);
        if reported.is_none() && effect.tail_exhausted() {
            reported = Some(frame);
        }
        if reported.is_some() {
            assert!(
                f64::from(wet[0].abs()) < FLOOR,
                "frame {frame}: the tail reported spent still sounds {}",
                wet[0]
            );
            assert!(
                effect.tail_exhausted(),
                "frame {frame}: a spent tail stays spent"
            );
        }
    }
    let reported = reported.expect("the delay reports its tail spent");
    assert!(
        reported <= bound,
        "the tail is reported spent {reported} frames after the impulse, past the bound {bound}"
    );
    effect.process([&[0.1], &[0.0]], [&mut wet, &mut right]);
    assert!(!effect.tail_exhausted(), "new input sounds again");
}

#[kithara::test]
fn feedback_delay_refuses_a_time_beyond_its_capacity() {
    let settings = Settings {
        capacity: 512,
        time: 256,
        feedback: 0.5,
        high_pass: f32::from(FeedbackHighPass::MIN),
    };
    assert_eq!(
        DelayTime::checked(0.0),
        None,
        "a zero delay is unrepresentable"
    );
    let effect = delay(settings);
    assert!(effect.accepts(&time(512.0)), "the full capacity is a delay");
    assert!(
        effect.accepts(&time(511.5)),
        "a fraction inside the capacity is a delay"
    );
    assert!(
        !effect.accepts(&time(512.5)),
        "a fraction past the capacity is refused"
    );
    assert!(
        !effect.accepts(&time(513.0)),
        "a delay past the capacity is refused"
    );
    assert!(effect.accepts(&FxParamChange::Delay(DelayParam::Feedback(Feedback::MAX))));
    assert!(effect.accepts(&FxParamChange::Delay(DelayParam::HighPass(
        FeedbackHighPass::MAX
    ))));
    assert!(
        FeedbackDelay::new(
            config(Settings {
                time: 513,
                ..settings
            }),
            &pools()
        )
        .is_err(),
        "a delay prepared past its capacity is refused"
    );
}

#[kithara::test]
fn feedback_delay_reset_returns_to_silence_and_keeps_its_settings() {
    let settings = Settings {
        capacity: 512,
        time: 300,
        feedback: 0.5,
        high_pass: f32::from(FeedbackHighPass::MIN),
    };
    let input = impulses(2048);
    let mut used = delay(settings);
    run(&mut used, &input, &REGULAR, &[(100, time(200.0))]);
    used.reset();
    assert!(used.tail_exhausted(), "a reset delay holds silence");
    let fresh = run(
        &mut delay(Settings {
            time: 200,
            ..settings
        }),
        &input,
        &REGULAR,
        &[],
    );
    assert_eq!(
        run(&mut used, &input, &REGULAR, &[])
            .each_ref()
            .map(|plane| bits(plane)),
        fresh.each_ref().map(|plane| bits(plane)),
        "a reset delay plays as one prepared at the time it was set to"
    );
}
