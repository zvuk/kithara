use std::num::NonZeroUsize;

use kithara_test_utils::{bufpool::pools, kithara};
use num_traits::ToPrimitive;

use super::{CombError, DelayError, FeedbackComb};
use crate::filter::{Coefficients, FilterError};

type Comb = FeedbackComb<2>;
type Planes = [Vec<f32>; 2];
type Planes64 = [Vec<f64>; 2];

const CAPACITY: NonZeroUsize = NonZeroUsize::MIN.saturating_add(399);
const CROSS: NonZeroUsize = NonZeroUsize::MIN.saturating_add(47);
const DELAY: f64 = 150.25;
const GAIN: f32 = 0.5;
/// The -120 dBFS floor at or under which a written sample counts as silent.
const FLOOR: f32 = 1e-6;
/// How far the comb may sit from the `f64` model of its loop.
const MODEL: f64 = 1e-5;
const REGULAR: [usize; 1] = [64];
const IRREGULAR: [usize; 6] = [37, 64, 13, 50, 1, 63];

/// A band-pass section whose two poles are a conjugate pair of radius 0.9;
/// its peak gain stays near one, so [`GAIN`] keeps the loop stable.
fn resonant() -> Coefficients<f64> {
    let (radius, angle) = (0.9_f64, 0.3_f64);
    Coefficients {
        a1: -2.0 * radius * angle.cos(),
        a2: radius * radius,
        b0: 0.1,
        b1: 0.0,
        b2: -0.1,
    }
}

/// A one-pole low-pass of unity DC gain whose pole sits at 0.8.
fn one_pole() -> Coefficients<f64> {
    Coefficients {
        a1: -0.8,
        a2: 0.0,
        b0: 0.2,
        b1: 0.0,
        b2: 0.0,
    }
}

fn comb_at(delay: f64, section: Coefficients<f64>, gain: f32) -> Comb {
    Comb::new(CAPACITY, delay, CROSS, section, gain, &pools()).expect("the comb fits the pools")
}

fn comb(section: Coefficients<f64>, gain: f32) -> Comb {
    comb_at(DELAY, section, gain)
}

fn f64_of(frame: usize) -> f64 {
    f64::from(u32::try_from(frame).expect("a test frame fits u32"))
}

fn f32_of(value: f64) -> f32 {
    value.to_f32().expect("a test sample fits f32")
}

/// Planes of `length` frames whose frame `frame` of channel `channel` is
/// `at(channel, frame)`.
fn planes(length: usize, at: impl Fn(usize, usize) -> f32) -> Planes {
    [0, 1].map(|channel| (0..length).map(|frame| at(channel, frame)).collect())
}

/// A unit impulse on the left, a smaller inverted one three frames later on
/// the right, then a short burst on both, then silence.
fn signal(length: usize) -> Planes {
    planes(length, |channel, frame| match (channel, frame) {
        (0, 0) => 1.0,
        (1, 3) => -0.5,
        (0, 40..60) => f32_of(0.4 * (f64_of(frame) * 0.7).sin()),
        (1, 40..60) => f32_of(0.3 * (f64_of(frame) * 0.7).cos()),
        _ => 0.0,
    })
}

/// A change `comb` takes on a frame.
#[derive(Clone, Copy, Debug)]
enum Change {
    Delay(f64),
    Cross(usize),
}

fn apply(comb: &mut Comb, change: Change) {
    match change {
        Change::Delay(frames) => comb.retime(comb.tap(frames).expect("a delay in range")),
        Change::Cross(frames) => comb.set_cross(NonZeroUsize::new(frames).expect("nonzero")),
    }
}

/// What `comb` makes of `input`, called in blocks of the sizes `blocks`
/// cycles through, each of `changes` taken on its frame.
fn run(comb: &mut Comb, input: &Planes, blocks: &[usize], changes: &[(usize, Change)]) -> Planes {
    let length = input[0].len();
    let mut output = planes(length, |_, _| 0.0);
    let mut changes = changes.iter().peekable();
    let mut start = 0;
    for block in blocks.iter().cycle() {
        if start == length {
            break;
        }
        while let Some((_, change)) = changes.next_if(|(at, _)| *at == start) {
            apply(comb, *change);
        }
        let due = changes.peek().map_or(length, |(at, _)| *at);
        let end = start.saturating_add(*block).min(due);
        comb.process(input, &mut output, start..end)
            .expect("both plane sets cover the block");
        start = end;
    }
    output
}

fn bits(planes: &Planes) -> [Vec<u32>; 2] {
    planes
        .each_ref()
        .map(|plane| plane.iter().map(|sample| sample.to_bits()).collect())
}

/// A biquad section in `f64`, direct form I, on coefficients rounded to
/// `f32` as the comb's section holds them.
#[derive(Clone, Copy)]
struct Section {
    coefficients: [f64; 5],
    state: [f64; 4],
}

impl Section {
    fn new(section: Coefficients<f64>) -> Self {
        let Coefficients { a1, a2, b0, b1, b2 } = section;
        Self {
            coefficients: [b0, b1, b2, a1, a2].map(|value| f64::from(f32_of(value))),
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

/// One channel of the comb in scalar `f64`.
struct Channel {
    history: Vec<f64>,
    output: Vec<f64>,
    section: Section,
}

impl Channel {
    /// The history `delay` frames back, as the chord toward the older frame.
    fn read(&self, delay: f64) -> f64 {
        let at = |back: usize| {
            self.history
                .len()
                .checked_sub(back)
                .and_then(|frame| self.history.get(frame))
                .copied()
                .unwrap_or(0.0)
        };
        let whole = delay.floor();
        let back = whole.to_usize().expect("a whole delay");
        let newer = at(back);
        newer + (delay - whole) * (at(back.saturating_add(1)) - newer)
    }
}

/// The comb in scalar `f64`: the output reads the history at the delay,
/// crossing linearly from the previous delay, and the history takes the input
/// plus the output through the section scaled by the gain. A delay change
/// during a cross waits for its end, the latest such change winning.
struct Model {
    channels: [Channel; 2],
    gain: f64,
    delay: f64,
    cross: Option<(f64, usize)>,
    cross_frames: usize,
    pending: Option<f64>,
}

impl Model {
    fn play(
        section: Coefficients<f64>,
        gain: f32,
        input: &Planes,
        changes: &[(usize, Change)],
    ) -> Planes64 {
        let mut model = Self {
            channels: [(); 2].map(|()| Channel {
                history: Vec::new(),
                output: Vec::new(),
                section: Section::new(section),
            }),
            gain: f64::from(gain),
            delay: DELAY,
            cross: None,
            cross_frames: CROSS.get(),
            pending: None,
        };
        let [left, right] = input;
        for (frame, (left, right)) in left.iter().zip(right).enumerate() {
            for (_, change) in changes.iter().filter(|(at, _)| *at == frame) {
                match *change {
                    Change::Delay(delay) => model.retime(delay),
                    Change::Cross(frames) => {
                        model.cross_frames = frames;
                        model.advance(0);
                    }
                }
            }
            model.step([*left, *right]);
        }
        model.channels.map(|channel| channel.output)
    }

    fn retime(&mut self, delay: f64) {
        if self.cross.is_some() {
            self.pending = Some(delay);
        } else if delay != self.delay {
            self.cross = Some((self.delay, 0));
            self.delay = delay;
        }
    }

    fn advance(&mut self, frames: usize) {
        let Some((from, done)) = self.cross else {
            return;
        };
        let done = done.saturating_add(frames);
        if done < self.cross_frames {
            self.cross = Some((from, done));
            return;
        }
        self.cross = None;
        if let Some(delay) = self.pending.take() {
            self.retime(delay);
        }
    }

    fn step(&mut self, input: [f32; 2]) {
        for (channel, input) in self.channels.iter_mut().zip(input) {
            let new = channel.read(self.delay);
            let output = self.cross.map_or(new, |(from, done)| {
                let old = channel.read(from);
                old + f64_of(done.saturating_add(1)) / f64_of(self.cross_frames) * (new - old)
            });
            let filtered = channel.section.step(output);
            channel
                .history
                .push(f64::from(input) + self.gain * filtered);
            channel.output.push(output);
        }
        self.advance(1);
    }
}

fn assert_near(actual: &Planes, expected: &Planes64, what: &str) {
    for (channel, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(actual.len(), expected.len(), "{what}: length");
        let worst = actual
            .iter()
            .zip(expected)
            .map(|(actual, expected)| (f64::from(*actual) - expected).abs())
            .enumerate()
            .max_by(|(_, left), (_, right)| left.total_cmp(right));
        assert!(
            worst.is_none_or(|(_, error)| error <= MODEL),
            "{what}, channel {channel}: worst frame and error {worst:?}"
        );
    }
}

/// Frames a comb waits after its last write above the floor before it has
/// rung out: the capacity, then the frames the slowest pole of its section,
/// of `radius`, takes to decay under the floor.
fn window(radius: f64) -> usize {
    let decay = (f64::from(FLOOR).ln() / radius.ln()).ceil();
    CAPACITY
        .get()
        .saturating_add(decay.to_usize().expect("a whole count of frames"))
}

#[kithara::test]
fn a_comb_follows_its_scalar_loop_in_any_partition() {
    let input = signal(2000);
    let regular = run(&mut comb(resonant(), GAIN), &input, &REGULAR, &[]);
    let irregular = run(&mut comb(resonant(), GAIN), &input, &IRREGULAR, &[]);
    assert_eq!(
        bits(&regular),
        bits(&irregular),
        "the comb runs the same in any block partition"
    );
    let [left, right] = &regular;
    assert!(
        left.iter()
            .take(150)
            .chain(right.iter().take(153))
            .all(|sample| *sample == 0.0),
        "nothing reads before the delay"
    );
    assert_near(
        &regular,
        &Model::play(resonant(), GAIN, &input, &[]),
        "the comb",
    );
}

#[kithara::test]
fn a_delay_change_crosses_and_the_latest_waiting_change_follows_it() {
    let input = signal(1200);
    let changes = [
        (300, Change::Delay(200.0)),
        (320, Change::Delay(90.0)),
        (330, Change::Delay(120.5)),
        (600, Change::Delay(120.5)),
    ];
    let regular = run(&mut comb(resonant(), GAIN), &input, &REGULAR, &changes);
    let irregular = run(&mut comb(resonant(), GAIN), &input, &IRREGULAR, &changes);
    assert_eq!(
        bits(&regular),
        bits(&irregular),
        "a cross lands the same in any block partition"
    );
    assert_near(
        &regular,
        &Model::play(resonant(), GAIN, &input, &changes),
        "the crosses",
    );
    let after_the_cross = [(300, Change::Delay(200.0)), (348, Change::Delay(120.5))];
    assert_near(
        &regular,
        &Model::play(resonant(), GAIN, &input, &after_the_cross),
        "the latest waiting change starts on the frame after the cross",
    );
}

#[kithara::test]
fn a_shorter_cross_ends_a_cross_already_past_it() {
    let input = signal(800);
    let changes = [
        (200, Change::Delay(260.0)),
        (210, Change::Delay(100.0)),
        (230, Change::Cross(20)),
    ];
    let output = run(&mut comb(one_pole(), GAIN), &input, &REGULAR, &changes);
    assert_near(
        &output,
        &Model::play(one_pole(), GAIN, &input, &changes),
        "the cross cut short",
    );
    let cut = [
        (200, Change::Delay(260.0)),
        (230, Change::Cross(20)),
        (230, Change::Delay(100.0)),
    ];
    assert_near(
        &output,
        &Model::play(one_pole(), GAIN, &input, &cut),
        "the waiting change starts where the shortened cross ends",
    );
}

#[kithara::test]
#[case::conjugate_pair(resonant(), 0.9)]
#[case::real_pole(one_pole(), 0.8)]
fn a_comb_rings_out_once_its_window_passes_without_a_loud_write(
    #[case] section: Coefficients<f64>,
    #[case] radius: f64,
) {
    const LOUD: usize = 5;
    let window = window(radius);
    let length = window.saturating_add(20);
    let input = planes(length, |channel, frame| match (channel, frame) {
        (0, 1) => FLOOR,
        (1, 2) => -FLOOR,
        (1, LOUD) => 0.25,
        _ => 0.0,
    });
    let mut output = planes(length, |_, _| 0.0);
    let mut comb = comb(section, 0.0);
    assert!(comb.rung_out(), "a fresh comb has rung out");
    for frame in 0..length {
        comb.process(&input, &mut output, frame..frame.saturating_add(1))
            .expect("both plane sets cover the frame");
        let expected = frame.checked_sub(LOUD).is_none_or(|quiet| quiet >= window);
        assert_eq!(
            comb.rung_out(),
            expected,
            "frame {frame}: samples at the floor stay silent and the right channel's write holds the comb for {window} frames"
        );
    }
    let loud = [vec![0.5_f32], vec![0.0]];
    let mut wet = [vec![0.0_f32], vec![0.0]];
    comb.process(&loud, &mut wet, 0..1).expect("one frame");
    assert!(!comb.rung_out(), "new input holds the comb again");
}

#[kithara::test]
fn a_reset_silences_the_comb_and_lands_the_waiting_change() {
    let mut played = comb(resonant(), GAIN);
    let changes = [(100, Change::Delay(200.0)), (110, Change::Delay(80.0))];
    run(&mut played, &signal(120), &REGULAR, &changes);
    played.reset();
    assert!(played.rung_out(), "a reset comb has rung out");
    let input = signal(700);
    assert_eq!(
        bits(&run(&mut played, &input, &IRREGULAR, &[])),
        bits(&run(
            &mut comb_at(80.0, resonant(), GAIN),
            &input,
            &IRREGULAR,
            &[]
        )),
        "a reset comb plays as a fresh one at the waiting delay"
    );
}

#[kithara::test]
fn a_mismatched_plane_set_is_refused_untouched() {
    let input = signal(256);
    let mut refused = comb(resonant(), GAIN);
    let [left, _] = &input;
    let mut output = planes(256, |_, _| 7.0);
    assert!(matches!(
        refused.process(&[left.as_slice()], &mut output, 0..64),
        Err(CombError::Delay(DelayError::Shape))
    ));
    let mut short = [vec![7.0_f32; 256], vec![7.0; 32]];
    assert!(matches!(
        refused.process(&input, &mut short, 0..64),
        Err(CombError::Delay(DelayError::Shape))
    ));
    assert!(
        output
            .iter()
            .chain(&short)
            .flatten()
            .all(|sample| *sample == 7.0),
        "nothing is written"
    );
    assert_eq!(
        bits(&run(&mut refused, &input, &REGULAR, &[])),
        bits(&run(&mut comb(resonant(), GAIN), &input, &REGULAR, &[])),
        "a refused call leaves the comb as it was"
    );
}

#[kithara::test]
fn an_unstable_section_or_a_delay_past_the_capacity_is_refused() {
    let input = signal(600);
    let mut kept = comb(resonant(), GAIN);
    let unstable = Coefficients {
        a2: 1.0,
        ..resonant()
    };
    assert_eq!(kept.retune(unstable), Err(FilterError::Parameters));
    assert_eq!(
        bits(&run(&mut kept, &input, &REGULAR, &[])),
        bits(&run(&mut comb(resonant(), GAIN), &input, &REGULAR, &[])),
        "the comb keeps the section it had"
    );
    assert!(matches!(
        Comb::new(CAPACITY, 400.5, CROSS, resonant(), GAIN, &pools()),
        Err(CombError::Delay(DelayError::OutOfRange))
    ));
    assert!(matches!(
        FeedbackComb::<0>::new(CAPACITY, DELAY, CROSS, resonant(), GAIN, &pools()),
        Err(CombError::Delay(DelayError::Shape))
    ));
}
