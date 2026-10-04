use std::{cell::RefCell, convert::Infallible, io::Error};

use kithara_config::{Config, ConfigOwner, Configure, LiveConfig};
use kithara_test_utils::kithara;

/// A gain of at most ten and a mute switch.
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(default, check(error = Error), fields(value, get(copy), builder(default)))]
struct Fader {
    #[config(live, check = Self::gain_bounds)]
    gain: u8,
    #[config(live)]
    muted: bool,
}

impl Fader {
    fn gain_bounds(gain: u8) -> Result<u8, Error> {
        if gain <= 10 {
            Ok(gain)
        } else {
            Err(Error::other("gain"))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(default, fields(value, get(copy), builder(default)))]
struct Looping {
    #[config(live)]
    bars: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(default, fields(value, get(copy), builder(default)))]
struct Tone {
    #[config(live)]
    pitch: i8,
}

/// A fader nested live beside a pan of its own.
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(default, check(error = Error), fields(value, get(copy), builder(default)))]
struct Strip {
    #[config(nested, live)]
    fader: Fader,
    #[config(live)]
    pan: i8,
}

/// A rate its owner executes itself beside a beat that goes the shared way.
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(default, fields(value, get(copy), builder(default)))]
struct Clock {
    #[config(live(owner))]
    rate: u32,
    #[config(live)]
    beat: u8,
}

/// Only owner-executed fields: its executor has no shared path to write.
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(default, fields(value, get(copy), builder(default)))]
struct Rate {
    #[config(live(owner))]
    hz: u32,
}

/// A live level beside the configuration it follows, whose type names it
/// `Self`.
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(owner_access, fields(value, get(copy)))]
struct Chain {
    #[config(live)]
    level: u8,
    #[config(get(ref))]
    next: Option<&'static Self>,
}

static FIRST: Chain = Chain {
    level: 1,
    next: None,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Moment {
    #[default]
    Now,
    At(u64),
}

/// One owner of three configurations whose field names do not overlap.
#[derive(Default)]
struct Desk {
    fader: RefCell<Fader>,
    looping: RefCell<Looping>,
    tone: RefCell<Tone>,
}

impl Configure<FaderChange> for Desk {
    type At = Moment;
    type Config = Fader;
    type Error = Error;
    type Output = Moment;

    fn configure(&self, change: FaderChange, at: Moment) -> Result<Moment, Error> {
        let change = Fader::check(change)?;
        self.fader.borrow_mut().apply_change(change);
        Ok(at)
    }

    fn settings(&self) -> Fader {
        *self.fader.borrow()
    }
}

impl Configure<LoopingChange> for Desk {
    type At = Moment;
    type Config = Looping;
    type Error = Infallible;
    type Output = Moment;

    fn configure(&self, change: LoopingChange, at: Moment) -> Result<Moment, Infallible> {
        let change = Looping::check(change)?;
        self.looping.borrow_mut().apply_change(change);
        Ok(at)
    }

    fn settings(&self) -> Looping {
        *self.looping.borrow()
    }
}

impl Configure<ToneChange> for Desk {
    type At = Moment;
    type Config = Tone;
    type Error = Infallible;
    type Output = Moment;

    fn configure(&self, change: ToneChange, at: Moment) -> Result<Moment, Infallible> {
        let change = Tone::check(change)?;
        self.tone.borrow_mut().apply_change(change);
        Ok(at)
    }

    fn settings(&self) -> Tone {
        *self.tone.borrow()
    }
}

/// An owner of a strip that records every change it is asked to execute.
#[derive(Default)]
struct Mixer {
    strip: RefCell<Strip>,
    asked: RefCell<Vec<String>>,
}

impl Configure<StripChange> for Mixer {
    type At = Moment;
    type Config = Strip;
    type Error = Error;
    type Output = ();

    fn configure(&self, change: StripChange, at: Moment) -> Result<(), Error> {
        let change = Strip::check(change)?;
        self.asked.borrow_mut().push(format!("{change:?} {at:?}"));
        self.strip.borrow_mut().apply_change(change);
        Ok(())
    }

    fn settings(&self) -> Strip {
        *self.strip.borrow()
    }
}

/// Owns a chain whose configuration only reads.
#[derive(ConfigOwner)]
#[config_owner(chain)]
struct Follower {
    chain: Chain,
}

impl Configure<ChainChange> for Follower {
    type At = Moment;
    type Config = Chain;
    type Error = Infallible;
    type Output = ();

    fn configure(&self, _: ChainChange, _: Moment) -> Result<(), Infallible> {
        Ok(())
    }

    fn settings(&self) -> Chain {
        self.chain
    }
}

/// Writes down which method each change reached.
struct Recorder;

impl ClockExec<Vec<String>> for Recorder {
    type At = u64;
    type Output = ();

    fn exec_live(&mut self, change: ClockChange, at: u64, cx: &mut Vec<String>) {
        cx.push(format!("live {change:?} at {at}"));
    }

    fn exec_rate(&mut self, rate: u32, at: u64, cx: &mut Vec<String>) {
        cx.push(format!("rate {rate} at {at}"));
    }
}

impl RateExec<Vec<String>> for Recorder {
    type At = u64;
    type Output = ();

    fn exec_hz(&mut self, hz: u32, at: u64, cx: &mut Vec<String>) {
        cx.push(format!("hz {hz} at {at}"));
    }
}

impl FaderExec<Vec<String>> for Recorder {
    type At = u64;
    type Output = ();

    fn exec_live(&mut self, change: FaderChange, at: u64, cx: &mut Vec<String>) {
        cx.push(format!("live {change:?} at {at}"));
    }
}

#[kithara::test]
fn setters_and_getters_of_three_configurations_resolve_by_field_name() {
    let desk = Desk::default();
    assert_eq!(desk.set_gain(3).expect("gain in bounds"), Moment::Now);
    assert_eq!(desk.set_bars(4).expect("nothing to refuse"), Moment::Now);
    assert_eq!(desk.set_pitch(-2).expect("nothing to refuse"), Moment::Now);
    assert_eq!((desk.gain(), desk.bars(), desk.pitch()), (3, 4, -2));
    assert!(!desk.muted());
}

#[kithara::test]
fn configure_carries_the_moment_the_caller_names() {
    let desk = Desk::default();
    let at = desk
        .configure(FaderChange::Muted(true), Moment::At(9))
        .expect("nothing to refuse");
    assert_eq!(at, Moment::At(9));
    assert!(desk.muted());
}

#[kithara::test]
fn a_setter_returns_the_owners_refusal_and_keeps_the_value() {
    let desk = Desk::default();
    desk.set_gain(6).expect("gain in bounds");
    let refusal = desk.set_gain(11).expect_err("gain over ten");
    assert_eq!(refusal.to_string(), "gain");
    assert_eq!(desk.gain(), 6);
}

#[kithara::test]
fn a_nested_setter_reaches_the_owner_as_the_parent_change() {
    let mixer = Mixer::default();
    mixer.fader().set_gain(4).expect("gain in bounds");
    mixer.set_pan(-1).expect("nothing to refuse");
    assert_eq!(*mixer.asked.borrow(), ["Fader(Gain(4)) Now", "Pan(-1) Now"]);
    assert_eq!((mixer.fader().gain(), mixer.pan()), (4, -1));
}

#[kithara::test]
fn a_nested_setter_is_refused_by_the_nested_check() {
    let mixer = Mixer::default();
    let refusal = mixer.fader().set_gain(11).expect_err("gain over ten");
    assert_eq!(refusal.to_string(), "gain");
    assert!(mixer.asked.borrow().is_empty(), "nothing reached the owner");
    assert_eq!(mixer.fader().gain(), 0);
}

#[kithara::test]
fn exec_routes_owner_fields_to_their_method_and_the_rest_to_exec_live() {
    let mut log = Vec::new();
    ClockExec::exec(&mut Recorder, ClockChange::Rate(48), 3, &mut log);
    ClockExec::exec(&mut Recorder, ClockChange::Beat(2), 5, &mut log);
    RateExec::exec(&mut Recorder, RateChange::Hz(96), 7, &mut log);
    FaderExec::exec(&mut Recorder, FaderChange::Muted(true), 9, &mut log);
    assert_eq!(
        log,
        [
            "rate 48 at 3",
            "live Beat(2) at 5",
            "hz 96 at 7",
            "live Muted(true) at 9",
        ]
    );
}

#[kithara::test]
fn a_field_type_naming_self_reads_as_the_configuration_in_every_generated_item() {
    let follower = Follower {
        chain: Chain {
            level: 2,
            next: Some(&FIRST),
        },
    };
    assert_eq!(ChainControl::next(&follower).map(Chain::level), Some(1));
    assert_eq!(ChainOwnerAccess::next(&follower).map(Chain::level), Some(1));
    assert_eq!(follower.chain.values().next.map(Chain::level), Some(1));
}

/// Generated bodies beside constants named like their bindings.
mod shadowed {
    use std::f32::consts::{E as value, LN_2 as cx, PI as at, SQRT_2 as config, TAU as change};

    use kithara_config::{Config, LiveConfig};
    use kithara_test_utils::kithara;

    use super::Tone;

    #[derive(Clone, Copy, Debug, PartialEq, Config)]
    #[config(builder(none), fields(value, get(copy)))]
    struct Knob {
        #[config(live(owner))]
        turn: f32,
        #[config(live)]
        glow: f32,
        #[config(nested, live)]
        tone: Tone,
    }

    /// Applies the shared fields to its knob and writes each change down.
    struct Turner(Knob);

    impl KnobExec<Vec<String>> for Turner {
        type At = f32;
        type Output = ();

        fn exec_live(&mut self, knob: KnobChange, moment: f32, log: &mut Vec<String>) {
            self.0.apply_change(knob);
            log.push(format!("glow {} {moment}", self.0.glow()));
        }

        fn exec_turn(&mut self, turn: f32, moment: f32, log: &mut Vec<String>) {
            log.push(format!("turn {turn} {moment}"));
        }
    }

    #[kithara::test]
    fn generated_bindings_ignore_constants_named_like_them() {
        let mut turner = Turner(Knob {
            turn: 0.0,
            glow: 0.0,
            tone: Tone::default(),
        });
        let mut log = Vec::new();
        KnobExec::exec(&mut turner, KnobChange::Turn(value), at, &mut log);
        KnobExec::exec(&mut turner, KnobChange::Glow(change), cx, &mut log);
        KnobExec::exec(&mut turner, KnobChange::Turn(config), value, &mut log);
        assert_eq!(
            log,
            [
                format!("turn {value} {at}"),
                format!("glow {change} {cx}"),
                format!("turn {config} {value}"),
            ]
        );
    }
}
