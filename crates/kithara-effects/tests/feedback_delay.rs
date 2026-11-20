#![forbid(unsafe_code)]

//! Public feedback-delay contract against an independent scalar loop.

use std::{
    f64::consts::{FRAC_1_SQRT_2, PI},
    num::{NonZeroU32, NonZeroUsize},
};

use kithara_effects::{
    DeckEffect, FxParamChange, FxParamMapping, FxParamUnit,
    delay::{DelayTime, Feedback, FeedbackDelay, FeedbackDelayConfig, FeedbackHighPass},
};
use kithara_test_utils::{bufpool::pools, kithara, kithara_platform::time::Duration};
use num_traits::ToPrimitive;

const RATE: u32 = 44_100;
/// Frames a time change crosses over in: the declared 20 ms at [`RATE`].
const RAMP: u32 = 882;
/// How far the effect may sit from the `f64` model of its loop.
const MODEL: f64 = 1e-6;
const REGULAR: [usize; 1] = [64];

#[derive(Clone, Copy)]
struct Settings {
    capacity: usize,
    time: u32,
    feedback: f32,
    high_pass: f32,
}

fn settings(time: u32, capacity: usize, feedback: f32, high_pass: f32) -> Settings {
    Settings {
        capacity,
        time,
        feedback,
        high_pass,
    }
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
    FxParamChange {
        index: 0,
        value: f64::from(DelayTime::checked(frames).expect("a representable delay")),
    }
}

fn high_pass_change(cutoff: f32) -> FxParamChange {
    FxParamChange {
        index: 2,
        value: f64::from(f32::from(
            FeedbackHighPass::checked(cutoff).expect("a cutoff in range"),
        )),
    }
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
            assert!(
                effect.parameters()[change.index].contains(change.value),
                "the delay takes {change:?}"
            );
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

fn takes(
    effect: &mut FeedbackDelay,
    input: &[Vec<f32>; 2],
    mut before: impl FnMut(usize, &mut FeedbackDelay),
) -> ([Vec<u32>; 2], Vec<bool>) {
    let length = input[0].len();
    let mut wet = [vec![0.0; length], vec![0.0; length]];
    let mut spent = Vec::new();
    for start in (0..length).step_by(REGULAR[0]) {
        before(start, effect);
        let end = (start + REGULAR[0]).min(length);
        let [left, right] = &mut wet;
        effect.process(
            [&input[0][start..end], &input[1][start..end]],
            [&mut left[start..end], &mut right[start..end]],
        );
        spent.push(effect.tail_exhausted());
    }
    (wet.map(|plane| bits(&plane)), spent)
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

fn high_pass(cutoff: impl Into<f64>, rate: u32) -> [f64; 3] {
    let g = (PI * cutoff.into() / f64::from(rate)).tan();
    let k = FRAC_1_SQRT_2.recip();
    [g, k, (1.0 + g * (g + k)).recip()]
}

struct Section {
    coefficients: [f64; 3],
    state: [f64; 2],
    quiet: usize,
}

impl Section {
    fn new(cutoff: f32, rate: u32) -> Self {
        Self {
            coefficients: high_pass(cutoff, rate),
            state: [0.0; 2],
            quiet: 0,
        }
    }

    fn step(&mut self, input: f64) -> f64 {
        let [g, k, scale] = self.coefficients;
        let [s1, s2] = self.state;
        let band = (s1 + g * (input - s2)) * scale;
        let low = s2 + g * band;
        self.state = [2.0 * band - s1, 2.0 * low - s2];
        let output = input - k * band - low;
        self.quiet = if input.abs() <= 1e-9 && output.abs() <= 1e-9 {
            self.quiet + 1
        } else {
            0
        };
        if self.quiet >= 2 {
            self.state = [0.0; 2];
        }
        output
    }
}

struct Model {
    wet: Vec<f64>,
    history: Vec<f64>,
    filter: Section,
    feedback: f64,
    time: usize,
    ramp: Option<(usize, u32)>,
}

impl Model {
    fn new(settings: Settings) -> Self {
        Self {
            wet: Vec::new(),
            history: Vec::new(),
            filter: Section::new(settings.high_pass, RATE),
            feedback: f64::from(settings.feedback),
            time: frames(settings.time),
            ramp: None,
        }
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

fn impulses(length: usize) -> [Vec<f32>; 2] {
    let mut input = [vec![0.0; length], vec![0.0; length]];
    input[0][0] = 1.0;
    input[1][3] = -0.5;
    input
}

fn sines(length: usize) -> [Vec<f32>; 2] {
    [3000.0, 2000.0].map(|frequency| {
        (0..length)
            .map(|frame| {
                sample(0.5 * (2.0 * PI * frequency * index(frame) / f64::from(RATE)).sin())
            })
            .collect()
    })
}

#[kithara::test]
fn feedback_delay_refuses_a_time_beyond_its_capacity() {
    let settings = settings(256, 512, 0.5, f32::from(FeedbackHighPass::MIN));
    assert_eq!(
        DelayTime::checked(0.0),
        None,
        "a zero delay is unrepresentable"
    );
    let effect = delay(settings);
    assert_eq!(effect.parameters().len(), 3);
    let domains = [
        (
            f64::from(DelayTime::MIN)..=512.0,
            FxParamUnit::Frames,
            FxParamMapping::Linear,
        ),
        (
            f64::from(f32::from(Feedback::MIN))..=f64::from(f32::from(Feedback::MAX)),
            FxParamUnit::Ratio,
            FxParamMapping::Linear,
        ),
        (
            f64::from(f32::from(FeedbackHighPass::MIN))
                ..=f64::from(f32::from(FeedbackHighPass::MAX)),
            FxParamUnit::Hertz,
            FxParamMapping::Log,
        ),
    ];
    for (actual, (range, unit, mapping)) in effect.parameters().iter().zip(domains) {
        assert_eq!(actual.range(), &range);
        assert_eq!(actual.unit(), unit);
        assert_eq!(actual.mapping(), Some(mapping));
    }
    assert!(
        effect.parameters()[0].contains(512.0),
        "the full capacity is a delay"
    );
    assert!(
        effect.parameters()[0].contains(511.5),
        "a fraction inside the capacity is a delay"
    );
    assert!(
        !effect.parameters()[0].contains(512.5),
        "a fraction past the capacity is refused"
    );
    assert!(
        !effect.parameters()[0].contains(513.0),
        "a delay past the capacity is refused"
    );
    assert!(
        f32::from(Feedback::MAX) < 1.0,
        "the feedback stays below unity, so every tail ends"
    );
    assert!(effect.parameters()[1].contains(f64::from(f32::from(Feedback::MAX))));
    assert!(effect.parameters()[2].contains(f64::from(f32::from(FeedbackHighPass::MAX))));
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
    let settings = settings(300, 512, 0.5, f32::from(FeedbackHighPass::MIN));
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
    let fresh = fresh.each_ref().map(|plane| bits(plane));
    assert_eq!(
        run(&mut used, &input, &REGULAR, &[])
            .each_ref()
            .map(|plane| bits(plane)),
        fresh,
        "a reset delay plays as one prepared at the time it was set to"
    );
    // A unit switched off takes changes without playing: the second waits for
    // the cross the first starts.
    let mut off = delay(settings);
    run(&mut off, &input, &REGULAR, &[]);
    off.set(time(250.0));
    off.set(time(200.0));
    off.reset();
    assert_eq!(
        run(&mut off, &input, &REGULAR, &[])
            .each_ref()
            .map(|plane| bits(plane)),
        fresh,
        "a reset drops the cross and lands the change waiting on it"
    );
}

#[kithara::test]
fn feedback_delay_tuned_to_a_rate_plays_as_one_prepared_at_it() {
    const LENGTH: usize = 32_768;
    const CHANGE: usize = 2048;
    let settings = settings(300, 1024, 0.5, 1000.0);
    let input = impulses(LENGTH);
    // The wet signal and the tail report of `effect`, its time crossing to 700
    // frames at `CHANGE`.
    let play = |effect: &mut FeedbackDelay| {
        takes(effect, &input, |start, effect| {
            if start == CHANGE {
                effect.set(time(700.0));
            }
        })
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
    /// Frames into the first echo when the rate changes, the high-pass ringing.
    const INTO: usize = 8;
    let settings = settings(TIME, frames(TIME), FEEDBACK, 1000.0);
    let time = frames(TIME);
    let split = time + INTO;
    let input = impulses(3 * time);
    let mut effect = delay(settings);
    let head = [&input[0][..split], &input[1][..split]].map(<[f32]>::to_vec);
    let tail = [&input[0][split..], &input[1][split..]].map(<[f32]>::to_vec);
    let mut wet = run(&mut effect, &head, &REGULAR, &[]);
    effect.update_sample_rate(rate_of(NEW));
    assert!(
        !effect.tail_exhausted(),
        "the echo sounding through the change keeps the tail alive"
    );
    for (wet, rest) in wet.iter_mut().zip(run(&mut effect, &tail, &REGULAR, &[])) {
        wet.extend(rest);
    }
    let [left, right] = &wet;
    assert!(
        left[..time].iter().all(|sample| *sample == 0.0),
        "nothing sounds before the first echo"
    );
    assert_eq!(left[time], 1.0, "the first echo is the impulse itself");
    assert_eq!(right[time + 3], -0.5, "each channel echoes its own input");
    for (channel, wet) in wet.iter().enumerate() {
        // The high-pass keeps its state and takes the coefficients of the new
        // rate from the frame the change lands on.
        let mut filter = Section::new(settings.high_pass, RATE);
        let expected: Vec<f64> = wet[time..2 * time]
            .iter()
            .enumerate()
            .map(|(frame, sample)| {
                if time + frame == split {
                    filter.coefficients = high_pass(settings.high_pass, NEW);
                }
                f64::from(FEEDBACK) * filter.step(f64::from(*sample))
            })
            .collect();
        assert_near(
            &wet[2 * time..],
            &expected,
            &format!(
                "channel {channel}: the history written on both sides of the change echoes one \
                 delay later, its high-pass retuned on the change's frame with its state kept"
            ),
        );
    }
}

#[kithara::test]
#[case::construction(8000, false)]
#[case::retune(RATE, false)]
#[case::set(8000, true)]
fn feedback_delay_saturates_its_design_and_restores_the_requested_cutoff(
    #[case] start: u32,
    #[case] set: bool,
) {
    let settings = settings(64, 64, 0.5, f32::from(FeedbackHighPass::MAX));
    let mut effect = delay_at(
        Settings {
            high_pass: if set { 20.0 } else { settings.high_pass },
            ..settings
        },
        start,
    );
    if set {
        effect.set(high_pass_change(settings.high_pass));
    }
    let parameters = effect.parameters().to_vec();
    let input = impulses(3 * frames(settings.time));
    let mut models = [Model::new(settings), Model::new(settings)];
    for (rate, cutoff) in [(8000, 4000.0_f64.next_down()), (48_000, 4000.0)] {
        effect.update_sample_rate(rate_of(rate));
        assert_eq!(
            effect.parameters(),
            parameters.as_slice(),
            "rate-independent domains"
        );
        let wet = run(&mut effect, &input, &REGULAR, &[]);
        for ((wet, input), model) in wet.iter().zip(&input).zip(&mut models) {
            let offset = model.wet.len();
            model.filter.coefficients = high_pass(cutoff, rate);
            for sample in input {
                model.step(*sample);
            }
            assert_near(
                wet,
                &model.wet[offset..],
                "saturated then restored TPT echo",
            );
        }
        assert_eq!(
            effect.high_pass(),
            FeedbackHighPass::MAX,
            "requested cutoff"
        );
        assert!(effect.parameters()[2].contains(f64::from(settings.high_pass)));
    }
}

#[kithara::test(timeout(Duration::from_secs(1)))]
fn feedback_delay_rate_drop_ends_a_cross_past_its_new_length() {
    const FROM: u32 = 1000;
    const TO: u32 = 1007;
    const CHANGE: usize = 2048;
    /// Frames into the 20 ms cross at [`RATE`] when the rate halves: past the
    /// 441 the cross lasts at the new rate.
    const DONE: usize = 640;
    let settings = settings(FROM, 2048, 0.0, f32::from(FeedbackHighPass::MAX));
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

#[kithara::test]
#[case::construction(false)]
#[case::retune(true)]
fn feedback_delay_high_pass_boundaries_and_former_holes(#[case] retune: bool) {
    let boundaries = [
        384_000, 768_000, 352_800, 705_600, 44_100, 48_000, 96_000, 192_000, 8_000, 326_461,
        326_462,
    ]
    .into_iter()
    .flat_map(|rate| [20.0, 4000.0].map(|cutoff| (rate, cutoff)));
    for (rate, cutoff) in boundaries.chain([
        (352_800, 20.022_65),
        (384_000, 20.0),
        (768_000, 20.0),
        (8000, 3_999.676_5),
    ]) {
        if f64::from(cutoff) >= f64::from(rate) / 2.0 {
            continue;
        }
        let settings = settings(1, 8, 0.5, cutoff);
        let mut effect = delay_at(settings, if retune { RATE } else { rate });
        if retune {
            effect.update_sample_rate(rate_of(rate));
        }
        assert!(effect.parameters()[2].contains(f64::from(cutoff)));
        let input = [1.0, 0.0, 0.0];
        let mut wet = [[0.0; 3]; 2];
        let [left, right] = &mut wet;
        effect.process([&input, &input], [left, right]);
        let expected = 0.5 * high_pass(cutoff, rate)[2];
        for plane in wet {
            assert_eq!(plane[1], 1.0, "first echo remains unfiltered");
            assert!(
                (f64::from(plane[2]) - expected).abs() <= MODEL,
                "new-rate echo at {rate}, cutoff {cutoff}, retune={retune}: {} vs {expected}",
                plane[2]
            );
        }
    }
}

#[kithara::test]
fn feedback_delay_refuses_a_time_past_its_domain_even_with_spare_capacity() {
    let oversized = f64::from(DelayTime::MAX) + 1.0;
    let capacity = oversized.to_usize().expect("capacity fits usize");
    let mut effect = delay(settings(1, capacity, 0.0, 1000.0));
    assert!(effect.parameters()[0].contains(f64::from(DelayTime::MAX)));
    assert!(!effect.parameters()[0].contains(oversized));
    effect.set(FxParamChange {
        index: 0,
        value: oversized,
    });

    let input = [1.0, 0.0];
    let mut wet = [[0.0; 2]; 2];
    let [left, right] = &mut wet;
    effect.process([&input; 2], [left, right]);
    assert_eq!(
        wet.map(|plane| plane.map(f32::to_bits)),
        [[0.0_f32.to_bits(), 1.0_f32.to_bits()]; 2]
    );
}
