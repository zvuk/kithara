use std::io::Error;

use kithara_config::{CheckedConfig, Config, ConfigOwner, ConfigOwnerMut, LiveConfig};
use kithara_test_utils::kithara;

/// A level of at most four under a limit of at most nine.
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(default, owner_access, check(error = Error), fields(value, get(copy)))]
struct Gauge {
    #[config(live, check = Self::level_bounds, builder(default = 2))]
    level: u8,
    #[config(check = Self::limit_bounds, builder(default = 9))]
    limit: u8,
}

impl Gauge {
    fn level_bounds(level: u8) -> Result<u8, Error> {
        if level <= 4 {
            Ok(level)
        } else {
            Err(Error::other("level"))
        }
    }

    fn limit_bounds(limit: u8) -> Result<u8, Error> {
        if limit <= 9 {
            Ok(limit)
        } else {
            Err(Error::other("limit"))
        }
    }
}

/// A gauge nested live beside a gain of its own.
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(default, check(error = Error), fields(value, get(copy)))]
struct Rig {
    #[config(nested, live, builder(default))]
    gauge: Gauge,
    #[config(live, builder(default = 1))]
    gain: u8,
}

/// A live config without checks, nested in another one.
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(default, fields(value, get(copy), builder(default)))]
struct Pan {
    #[config(live)]
    pan: i8,
}

#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(default, fields(value, get(copy), builder(default)))]
struct Strip {
    #[config(nested, live)]
    pan: Pan,
    #[config(live)]
    mute: bool,
}

/// A live field of a `Copy` type other than a number.
#[derive(Clone, Copy, Config)]
#[config(builder(none))]
struct Label {
    #[config(value, live)]
    text: &'static str,
}

fn value(level: u8) -> Result<u8, Error> {
    if level <= 1 {
        Ok(level)
    } else {
        Err(Error::other("value"))
    }
}

fn change(level: u8) -> Result<u8, Error> {
    if level <= 1 {
        Ok(level)
    } else {
        Err(Error::other("change"))
    }
}

/// Field checks named like the locals the derive generates around them.
#[derive(Clone, Copy, Config)]
#[config(builder(none), check(error = Error))]
struct Shadowed {
    #[config(value, live, check = value)]
    first: u8,
    #[config(value, live, check = change)]
    second: u8,
}

/// A live field whose name is a keyword.
#[derive(Clone, Copy, Config)]
#[config(builder(none))]
struct Kind {
    #[config(value, live)]
    r#type: u8,
}

#[derive(ConfigOwner)]
#[config_owner(config)]
#[config_owner_mut]
struct Owner {
    config: Gauge,
}

#[derive(ConfigOwner)]
#[config_owner(Gauge, inner.config)]
struct NestedOwner {
    inner: std::sync::Arc<Owner>,
}

/// An owner whose configuration is whatever its field owns.
#[derive(ConfigOwner)]
#[config_owner(delegate(owner))]
#[config_owner_mut]
struct Delegating {
    owner: Owner,
}

fn refusal<T: core::fmt::Debug>(result: Result<T, Error>) -> String {
    result.expect_err("the check refuses the value").to_string()
}

#[kithara::test]
fn validation_checks_every_field_in_declaration_order() {
    let both = Gauge {
        level: 5,
        limit: 10,
    };
    assert_eq!(
        refusal(both.validated()),
        "level",
        "the first field is first"
    );
    let limit = Gauge {
        level: 3,
        limit: 10,
    };
    assert_eq!(refusal(limit.validated()), "limit");
    let valid = Gauge { level: 3, limit: 9 };
    assert_eq!(valid.validated().expect("both fields in bounds"), valid);
}

#[kithara::test]
fn a_change_passes_only_its_own_field_check() {
    assert_eq!(refusal(Gauge::check(GaugeChange::Level(5))), "level");
    assert!(matches!(
        Gauge::check(GaugeChange::Level(4)),
        Ok(GaugeChange::Level(4))
    ));
}

#[kithara::test]
fn a_change_assigns_exactly_its_field() {
    let mut gauge = Gauge { level: 2, limit: 7 };
    gauge.apply_change(GaugeChange::Level(3));
    assert_eq!(gauge, Gauge { level: 3, limit: 7 });
    gauge.apply_change(GaugeChange::Level(9));
    assert_eq!(gauge.level, 9, "applying never checks; the sender did");

    let mut label = Label { text: "intro" };
    label.apply_change(LabelChange::Text("drop"));
    assert_eq!(label.text, "drop");
}

#[kithara::test]
fn a_field_check_named_like_a_generated_local_still_runs() {
    assert_eq!(refusal(Shadowed::check(ShadowedChange::First(2))), "value");
    assert_eq!(
        refusal(Shadowed::check(ShadowedChange::Second(2))),
        "change"
    );
    let mut kind = Kind { r#type: 0 };
    kind.apply_change(KindChange::Type(3));
    assert_eq!(kind.r#type, 3);
}

#[kithara::test]
fn a_nested_config_is_validated_inside_its_parent() {
    let rig = Rig {
        gauge: Gauge { level: 5, limit: 9 },
        gain: 1,
    };
    assert_eq!(refusal(rig.validated()), "level");
    assert_eq!(
        Rig::default().validated().expect("defaults are valid"),
        Rig::default()
    );
}

#[kithara::test]
fn a_nested_change_is_checked_and_applied_through_its_parent() {
    assert_eq!(
        refusal(Rig::check(RigChange::from(GaugeChange::Level(5)))),
        "level"
    );
    let change = Rig::check(GaugeChange::Level(3).into()).expect("level in bounds");
    let mut rig = Rig::default();
    rig.apply_change(change);
    rig.apply_change(RigChange::Gain(6));
    assert_eq!(
        rig,
        Rig {
            gauge: Gauge { level: 3, limit: 9 },
            gain: 6,
        }
    );
}

#[kithara::test]
fn a_config_without_checks_refuses_nothing_and_still_nests() {
    let mut strip = Strip::default();
    let change = Strip::check(PanChange::Pan(-3).into()).expect("nothing to refuse");
    strip.apply_change(change);
    strip.apply_change(StripChange::Mute(true));
    assert_eq!(
        strip,
        Strip {
            pan: Pan { pan: -3 },
            mute: true,
        }
    );
    assert_eq!(strip.validated().expect("nothing to refuse"), strip);
}

#[kithara::test]
fn an_owner_change_is_checked_and_a_refusal_keeps_the_value() {
    let mut owner = Owner {
        config: Gauge::default(),
    };
    owner
        .apply_config_change(GaugeChange::Level(3))
        .expect("level in bounds");
    assert_eq!(owner.level(), 3);
    assert_eq!(
        refusal(owner.apply_config_change(GaugeChange::Level(7))),
        "level"
    );
    assert_eq!(owner.level(), 3);

    let nested = NestedOwner {
        inner: std::sync::Arc::new(owner),
    };
    assert!(std::ptr::eq(nested.config(), &nested.inner.config));
    assert_eq!(nested.level(), 3);
}

#[kithara::test]
fn a_delegating_owner_reads_and_changes_the_config_its_field_owns() {
    let mut delegating = Delegating {
        owner: Owner {
            config: Gauge::default(),
        },
    };
    assert!(std::ptr::eq(delegating.config(), &delegating.owner.config));
    delegating
        .apply_config_change(GaugeChange::Level(4))
        .expect("level in bounds");
    assert_eq!((delegating.level(), delegating.owner.level()), (4, 4));
}
