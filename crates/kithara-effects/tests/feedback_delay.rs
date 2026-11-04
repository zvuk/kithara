#![forbid(unsafe_code)]

//! The feedback delay a deck FX unit holds: echo positions and amplitudes in
//! any block partition, the cross-read of a time change, the place of the
//! feedback filter in the loop, the reported tail, a new sample rate and the
//! refusals.

use std::{
    f64::consts::{FRAC_1_SQRT_2, PI},
    num::{NonZeroU32, NonZeroUsize},
    time::Duration,
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
/// The -120 dBFS floor at or under which a sample counts as silent.
const FLOOR: f32 = 1e-6;
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

fn config(settings: Settings, rate: u32) -> FeedbackDelayConfig {
    FeedbackDelayConfig::builder()
        .sample_rate(rate_of(rate))
        .capacity(NonZeroUsize::new(settings.capacity).expect("a nonzero capacity"))
        .time(DelayTime::checked(f64::from(settings.time)).expect("a representable delay"))
        .feedback(Feedback::checked(settings.feedback).expect("a feedback below unity"))
        .high_pass(FeedbackHighPass::checked(settings.high_pass).expect("a cutoff in range"))
        .build()
}

fn delay(settings: Settings) -> FeedbackDelay {
    delay_at(settings, RATE)
}

fn delay_at(settings: Settings, rate: u32) -> FeedbackDelay {
    FeedbackDelay::new(config(settings, rate), &pools()).expect("the delay fits the test pools")
}

fn rate_of(rate: u32) -> NonZeroU32 {
    NonZeroU32::new(rate).expect("a nonzero rate")
}

fn time(frames: f64) -> FxParamChange {
    FxParamChange::Delay(DelayParam::Time(
        DelayTime::checked(frames).expect("a representable delay"),
    ))
}

fn high_pass_change(cutoff: f32) -> FxParamChange {
    FxParamChange::Delay(DelayParam::HighPass(
        FeedbackHighPass::checked(cutoff).expect("a cutoff in range"),
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

/// The cookbook high-pass at `cutoff` for `rate` with Butterworth Q:
/// `[b0, b1, b2, a1, a2]`.
fn cookbook(cutoff: f32, rate: u32) -> [f64; 5] {
    let omega = 2.0 * PI * f64::from(cutoff) / f64::from(rate);
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
    .map(|coefficient| coefficient * scale)
}

/// The [`cookbook`] high-pass rounded to `f32`, as a filter running in `f32`
/// holds it.
fn high_pass(cutoff: f32, rate: u32) -> [f64; 5] {
    cookbook(cutoff, rate).map(|coefficient| f64::from(sample(coefficient)))
}

/// Frames a delay holding `capacity` frames, its high-pass at `cutoff` for
/// `rate`, waits after its last write above the floor before reporting its
/// tail spent: the capacity, then the frames the high-pass's slowest pole
/// takes to decay under the floor. The two poles are a conjugate pair of
/// radius `√a2`.
fn tail_window(capacity: usize, cutoff: f32, rate: u32) -> usize {
    let [.., a2] = cookbook(cutoff, rate);
    let decay = (f64::from(FLOOR).ln() / a2.sqrt().ln()).ceil();
    capacity + decay.to_usize().expect("a whole count of frames")
}

/// A biquad in `f64`, direct form I.
struct Section {
    coefficients: [f64; 5],
    state: [f64; 4],
}

impl Section {
    fn new(cutoff: f32, rate: u32) -> Self {
        Self {
            coefficients: high_pass(cutoff, rate),
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
            filter: Section::new(settings.high_pass, RATE),
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

/// A 3 kHz sine on the left and a 2 kHz one on the right, both at half scale.
fn sines(length: usize) -> [Vec<f32>; 2] {
    [3000.0, 2000.0].map(|frequency| {
        (0..length)
            .map(|frame| {
                sample(0.5 * (2.0 * PI * frequency * index(frame) / f64::from(RATE)).sin())
            })
            .collect()
    })
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
    let input = sines(length);
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
fn feedback_delay_time_change_during_a_cross_waits_for_its_end() {
    const FROM: u32 = 1000;
    const TO: u32 = 1007;
    /// A time asked for during the cross and replaced before it ends.
    const REPLACED: u32 = 1500;
    const LAST: u32 = 990;
    const CHANGE: usize = 6000 + 37;
    let settings = Settings {
        capacity: 2048,
        time: FROM,
        feedback: 0.5,
        high_pass: f32::from(FeedbackHighPass::MAX),
    };
    let during = CHANGE + frames(RAMP) / 2;
    let end = CHANGE + frames(RAMP);
    let input = sines(end + 2 * frames(RAMP) + 1000);
    let wet = run(
        &mut delay(settings),
        &input,
        &REGULAR,
        &[
            (CHANGE, time(f64::from(TO))),
            (during, time(f64::from(REPLACED))),
            (during, time(f64::from(LAST))),
        ],
    );
    for (channel, (wet, input)) in wet.iter().zip(&input).enumerate() {
        assert_near(
            wet,
            &Model::play(settings, input, &[(CHANGE, TO), (end, LAST)]).wet,
            &format!(
                "channel {channel}: the running cross completes, and the latest change asked \
                 for during it starts on the frame after it ends"
            ),
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
        let mut filter = Section::new(settings.high_pass, RATE);
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
    /// Frames between two readings of the report.
    const BLOCK: usize = 16;
    /// Frames after which a delay still not reporting its tail spent fails.
    const LIMIT: usize = 1 << 18;
    let settings = |time, capacity, feedback, high_pass| Settings {
        capacity,
        time,
        feedback,
        high_pass,
    };
    let low = f32::from(FeedbackHighPass::MIN);
    let high = f32::from(FeedbackHighPass::MAX);
    // Settings, rate, and a cutoff the delay moves to on a frame.
    let cases = [
        (settings(256, 512, 0.5, low), RATE, None),
        (settings(16, 64, 0.95, low), RATE, None),
        (settings(64, 128, 0.95, low), RATE, None),
        (settings(256, 512, 0.95, 200.0), RATE, None),
        (settings(64, 64, 0.5, high), RATE, None),
        (settings(256, 512, 0.5, low), 96_000, None),
        (settings(256, 512, 0.5, low), RATE, Some((3072, high))),
    ];
    for (case, (settings, rate, change)) in cases.into_iter().enumerate() {
        let mut effect = delay_at(settings, rate);
        assert!(
            effect.tail_exhausted(),
            "case {case}: a prepared delay holds silence"
        );
        let time = frames(settings.time);
        let mut window = tail_window(settings.capacity, settings.high_pass, rate);
        let mut wet = Vec::new();
        let mut reports = Vec::new();
        let mut first = None;
        while first.is_none_or(|first| wet.len() < first + window + time) {
            assert!(
                wet.len() < LIMIT,
                "case {case}: the delay never reports its tail spent"
            );
            if let Some((at, cutoff)) = change
                && at == wet.len()
            {
                effect.set(high_pass_change(cutoff));
                window = tail_window(settings.capacity, cutoff, rate);
            }
            let mut input = [0.0; BLOCK];
            if wet.is_empty() {
                input[0] = 1.0;
            }
            let mut left = [0.0; BLOCK];
            effect.process([&input, &[0.0; BLOCK]], [&mut left, &mut [0.0; BLOCK]]);
            wet.extend(left);
            let spent = effect.tail_exhausted();
            if spent && first.is_none() {
                first = Some(wet.len());
            }
            reports.push((wet.len(), spent, window));
        }
        // The history the delay wrote is its wet signal one delay later.
        let loud: Vec<bool> = wet[time..]
            .iter()
            .map(|sample| sample.abs() > FLOOR)
            .collect();
        let first = first.expect("the loop ends on a report");
        let mut last = None;
        let mut read = 0;
        for (written, spent, window) in reports {
            if written > loud.len() {
                break;
            }
            if let Some(frame) = loud[read..written].iter().rposition(|loud| *loud) {
                last = Some(read + frame);
            }
            read = written;
            assert_eq!(
                spent,
                last.is_none_or(|last| written - 1 - last >= window),
                "case {case}: after {written} frames, the last above the floor at {last:?}: \
                 the tail is spent exactly once the window of {window} frames holds none"
            );
            assert!(
                written < first || spent,
                "case {case}: after {written} frames, a spent tail stays spent"
            );
        }
        assert!(
            wet[first..].iter().all(|sample| sample.abs() <= FLOOR),
            "case {case}: the tail reported spent at {first} still sounds"
        );
        effect.process([&[0.1], &[0.0]], [&mut [0.0], &mut [0.0]]);
        assert!(
            !effect.tail_exhausted(),
            "case {case}: new input sounds again"
        );
    }
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
            config(
                Settings {
                    time: 513,
                    ..settings
                },
                RATE
            ),
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

#[kithara::test]
fn feedback_delay_tuned_to_a_rate_plays_as_one_prepared_at_it() {
    const LENGTH: usize = 32_768;
    const CHANGE: usize = 2048;
    let settings = Settings {
        capacity: 1024,
        time: 300,
        feedback: 0.5,
        high_pass: 1000.0,
    };
    let input = impulses(LENGTH);
    // The wet signal and the tail report after each block of `effect`, its
    // time crossing to 700 frames at `CHANGE`.
    let play = |effect: &mut FeedbackDelay| {
        let mut wet = [vec![0.0; LENGTH], vec![0.0; LENGTH]];
        let mut spent = Vec::new();
        for start in (0..LENGTH).step_by(REGULAR[0]) {
            if start == CHANGE {
                effect.set(time(700.0));
            }
            let end = start + REGULAR[0];
            let [left, right] = &mut wet;
            effect.process(
                [&input[0][start..end], &input[1][start..end]],
                [&mut left[start..end], &mut right[start..end]],
            );
            spent.push(effect.tail_exhausted());
        }
        (wet.map(|plane| bits(&plane)), spent)
    };
    for rate in [RATE, 22_050, 48_000, 96_000] {
        let mut tuned = delay(settings);
        tuned.update_sample_rate(rate_of(rate));
        let (wet, spent) = play(&mut tuned);
        let (expected_wet, expected_spent) = play(&mut delay_at(settings, rate));
        assert_eq!(
            wet, expected_wet,
            "at {rate} Hz the high-pass and the cross play as prepared there"
        );
        assert_eq!(
            spent, expected_spent,
            "at {rate} Hz the tail is reported as prepared there"
        );
        assert!(
            spent.first() == Some(&false) && spent.last() == Some(&true),
            "the takes at {rate} Hz cover the tail and its report"
        );
    }
}

#[kithara::test]
fn feedback_delay_keeps_its_history_and_time_across_a_rate_change() {
    const TIME: u32 = 1000;
    const NEW: u32 = 48_000;
    const FEEDBACK: f32 = 0.5;
    let settings = Settings {
        capacity: frames(TIME),
        time: TIME,
        feedback: FEEDBACK,
        high_pass: 1000.0,
    };
    let time = frames(TIME);
    let input = impulses(3 * time);
    let mut effect = delay(settings);
    let head = [&input[0][..time / 2], &input[1][..time / 2]].map(<[f32]>::to_vec);
    let tail = [&input[0][time / 2..], &input[1][time / 2..]].map(<[f32]>::to_vec);
    let mut wet = run(&mut effect, &head, &REGULAR, &[]);
    effect.update_sample_rate(rate_of(NEW));
    for (wet, rest) in wet.iter_mut().zip(run(&mut effect, &tail, &REGULAR, &[])) {
        wet.extend(rest);
    }
    let [left, right] = &wet;
    assert!(
        left[..time].iter().all(|sample| *sample == 0.0),
        "nothing sounds before the first echo"
    );
    assert_eq!(
        left[time], 1.0,
        "the impulse written before the change echoes the same frames later"
    );
    assert_eq!(right[time + 3], -0.5, "each channel keeps its history");
    let mut filter = Section::new(settings.high_pass, NEW);
    let expected: Vec<f64> = left[time..2 * time]
        .iter()
        .map(|sample| f64::from(FEEDBACK) * filter.step(f64::from(*sample)))
        .collect();
    assert_near(
        &left[2 * time..],
        &expected,
        "the next echo passes the high-pass tuned to the new rate",
    );
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn feedback_delay_rate_drop_ends_a_cross_past_its_new_length() {
    const FROM: u32 = 1000;
    const TO: u32 = 1007;
    const CHANGE: usize = 2048;
    /// Frames into the 20 ms cross at [`RATE`] when the rate halves: past the
    /// 441 the cross lasts at the new rate.
    const DONE: usize = 640;
    let settings = Settings {
        capacity: 2048,
        time: FROM,
        feedback: 0.0,
        high_pass: f32::from(FeedbackHighPass::MAX),
    };
    let input = sines(CHANGE + DONE + 2048);
    let split = CHANGE + DONE;
    let head = [&input[0][..split], &input[1][..split]].map(<[f32]>::to_vec);
    let tail = [&input[0][split..], &input[1][split..]].map(<[f32]>::to_vec);
    let mut effect = delay(settings);
    run(
        &mut effect,
        &head,
        &REGULAR,
        &[(CHANGE, time(f64::from(TO)))],
    );
    effect.update_sample_rate(rate_of(RATE / 2));
    let wet = run(&mut effect, &tail, &REGULAR, &[]);
    for (channel, (wet, input)) in wet.iter().zip(&input).enumerate() {
        assert_eq!(
            bits(wet),
            bits(&input[split - frames(TO)..input.len() - frames(TO)]),
            "channel {channel}: the cross ends with the rate drop and reads only the new time"
        );
    }
}
