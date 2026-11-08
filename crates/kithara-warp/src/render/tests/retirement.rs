use Control::{Backend, Keylock, Speed};
use kithara_stretch::StretchKind;
use kithara_sync::SpeedCurve;
use kithara_test_fixtures::unit_fixtures::warp_sine;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{exact::mapped_signal, renderer, spec};
use crate::WarpConfig;

/// Output frames at which a script presses.
const PRESSES: [usize; 2] = [22_050, 66_150];

/// Output quantum the lane renders with.
const PRESS_BUDGET: usize = 128;

/// Output frames a script plays after its last press.
const PRESS_TAIL: usize = 22_050;

/// Distance of a press that lands inside the previous press's fade.
const IN_FADE: usize = 512;

/// A lane control a press applies.
#[derive(Clone, Copy, Debug)]
enum Control {
    Keylock(bool),
    Speed(f32),
    Backend(StretchKind),
}

/// Controls the lane applies as one batch at an output frame. Presses at one
/// frame run as consecutive batches with no render between them.
#[derive(Debug)]
struct Press {
    frame: usize,
    batch: Vec<Control>,
}

/// A scripted lane run on a steady tone.
#[derive(Debug)]
struct Scenario {
    backend: StretchKind,
    keylock: bool,
    speed: f32,
    presses: Vec<Press>,
}

impl Scenario {
    /// Left channel of `tone` this script plays up to `PRESS_TAIL` frames
    /// after its last press.
    fn played(&self, tone: &[f32]) -> Vec<f32> {
        let frames = self.presses.last().map_or(0, |press| press.frame) + PRESS_TAIL;
        let config = config(self.backend, self.keylock, self.speed);
        render(&config, &self.presses, frames, tone)
    }
}

fn config(backend: StretchKind, keylock: bool, speed: f32) -> WarpConfig {
    WarpConfig::builder()
        .backend(backend)
        .keylock(keylock)
        .speed(speed)
        .build()
}

/// Left channel of `tone` rendered for `frames` output frames quantum by
/// quantum as the render lane renders it, with each press applied as a batch
/// at its frame.
#[kithara::hang_watchdog]
fn render(config: &WarpConfig, presses: &[Press], frames: usize, tone: &[f32]) -> Vec<f32> {
    let mut fx = renderer(config);
    let mut source = 0;
    let mut mono = Vec::new();
    let mut presses = presses.iter().peekable();
    while mono.len() < frames {
        hang_tick!();
        while let Some(press) = presses.next_if(|press| press.frame == mono.len()) {
            for control in &press.batch {
                match *control {
                    Keylock(on) => fx.set_keylock(on),
                    Speed(speed) => fx
                        .set_speed(SpeedCurve::Constant(speed), fx.render_revision() + 1)
                        .expect("valid speed"),
                    Backend(kind) => fx.set_backend(kind),
                }
            }
            fx.prepare_engine_latency(spec()).expect("prepared batch");
        }
        let due = presses.peek().map_or(PRESS_BUDGET, |press| {
            PRESS_BUDGET.min(press.frame - mono.len())
        });
        let output = mapped_signal(&mut fx, &mut source, due, |frame| {
            tone[usize::try_from(frame).expect("source frame fits usize") * 2 % tone.len()]
        });
        mono.extend(output.samples.iter().step_by(2));
        hang_reset!();
    }
    mono
}

/// Steps between adjacent frames.
fn steps(mono: &[f32]) -> impl Iterator<Item = f32> + '_ {
    mono.windows(2).map(|pair| (pair[1] - pair[0]).abs())
}

/// From the step into the first press on, every step stays within the
/// steepest steady side's step plus the gain slope of the shortest keylock
/// fade across both sides. The sides are every backend, keylock state and
/// speed of the script.
fn assert_continuous(tone: &[f32], scenario: &Scenario) {
    let mut backends = vec![scenario.backend];
    let mut speeds = vec![scenario.speed];
    for control in scenario.presses.iter().flat_map(|press| &press.batch) {
        match *control {
            Backend(kind) => backends.push(kind),
            Speed(speed) => speeds.push(speed),
            Keylock(_) => {}
        }
    }
    let sides: Vec<WarpConfig> = backends
        .iter()
        .flat_map(|&backend| {
            speeds
                .iter()
                .flat_map(move |&speed| [false, true].map(|on| config(backend, on, speed)))
        })
        .collect();
    let output = scenario.played(tone);
    let from = scenario.presses[0].frame - 1;
    let own_step = sides
        .iter()
        .map(|side| {
            let twin = render(side, &[], output.len(), tone);
            steps(&twin[from..]).fold(0.0, f32::max)
        })
        .fold(0.0, f32::max);
    let fade = sides
        .iter()
        .filter(|side| side.keylock())
        .map(|side| {
            renderer(side)
                .prepare_engine_latency(spec())
                .expect("latency")
                .get()
        })
        .filter(|&latency| latency > 0)
        .min()
        .expect("a keylock side off unity");
    assert!(
        IN_FADE < fade,
        "a press after IN_FADE lands inside the fade"
    );
    let peak = tone
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    let bound = own_step + 2.0 * peak / fade.to_f32().expect("fade fits f32");
    for (frame, step) in steps(&output[from..]).enumerate() {
        assert!(
            step <= bound,
            "{scenario:?}: step {step} into output frame {} exceeds {bound}",
            from + frame + 1
        );
    }
}

/// Every retirement a lane press causes fades the old side into the new one
/// without a click: keylock toggles both ways, a press and its release
/// inside the fade, keylock off inside a speed change's fade, and entry to
/// and exit from keylocked unity.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn retirement_keeps_the_tone_continuous(#[case] backend: StretchKind, warp_sine: Vec<f32>) {
    let press = |frame, batch: &[Control]| Press {
        frame,
        batch: batch.to_vec(),
    };
    let script = |keylock, speed, presses| Scenario {
        backend,
        keylock,
        speed,
        presses,
    };
    let mut scenarios = Vec::new();
    for speed in [1.08, 1.0167] {
        for keylock in [false, true] {
            for back in [PRESSES[1], PRESSES[0] + IN_FADE] {
                let presses = vec![
                    press(PRESSES[0], &[Keylock(!keylock)]),
                    press(back, &[Keylock(keylock)]),
                ];
                scenarios.push(script(keylock, speed, presses));
            }
        }
    }
    scenarios.extend([
        script(
            true,
            1.08,
            vec![
                press(PRESSES[0] - IN_FADE, &[Speed(1.5)]),
                press(PRESSES[0], &[Keylock(false)]),
            ],
        ),
        script(
            false,
            1.0,
            vec![
                press(PRESSES[0], &[Keylock(true)]),
                press(PRESSES[1], &[Speed(1.08)]),
            ],
        ),
        script(true, 1.08, vec![press(PRESSES[0], &[Speed(1.0)])]),
        script(
            false,
            1.0,
            vec![press(PRESSES[0], &[Keylock(true), Speed(1.08)])],
        ),
    ]);
    for scenario in &scenarios {
        assert_continuous(&warp_sine, scenario);
    }
}

/// A keylock backend change fades the source-supported prefix of the old
/// engine's tail into the new engine.
#[cfg(all(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
#[kithara::test]
fn keylock_backend_change_keeps_the_tone_continuous(warp_sine: Vec<f32>) {
    assert_continuous(
        &warp_sine,
        &Scenario {
            backend: StretchKind::Signalsmith,
            keylock: true,
            speed: 1.08,
            presses: vec![Press {
                frame: PRESSES[0],
                batch: vec![Backend(StretchKind::Bungee)],
            }],
        },
    );
}

/// Batches the lane applies before one render sound as their net batch: two
/// speed changes that leave and reenter a keylock speed, and a speed change
/// then keylock on from vinyl unity.
#[kithara::test]
#[cfg_attr(
    feature = "stretch-signalsmith",
    case::signalsmith(StretchKind::Signalsmith)
)]
#[cfg_attr(feature = "stretch-bungee", case::bungee(StretchKind::Bungee))]
fn batches_before_a_render_sound_as_their_net_batch(
    #[case] backend: StretchKind,
    warp_sine: Vec<f32>,
) {
    let at_press = |keylock, speed, batches: &[&[Control]]| Scenario {
        backend,
        keylock,
        speed,
        presses: batches
            .iter()
            .map(|batch| Press {
                frame: PRESSES[0],
                batch: batch.to_vec(),
            })
            .collect(),
    };
    let (on, tempo) = (Keylock(true), Speed(1.08));
    for (batches, net) in [
        (
            at_press(true, 1.08, &[&[Speed(1.0)], &[tempo]]),
            at_press(true, 1.08, &[&[tempo]]),
        ),
        (
            at_press(false, 1.0, &[&[tempo], &[on]]),
            at_press(false, 1.0, &[&[on, tempo]]),
        ),
    ] {
        let net_output = net.played(&warp_sine);
        let departure = batches
            .played(&warp_sine)
            .iter()
            .zip(&net_output)
            .position(|(split, joined)| split != joined);
        assert_eq!(departure, None, "{batches:?} departs from {net:?}");
    }
}
