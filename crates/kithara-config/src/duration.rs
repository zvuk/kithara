//! Serde adapters for the configuration duration syntax.

use std::fmt;

use fundu_core::{
    config::Config,
    parse::{DurationRepr, ReprParserMultiple, ReprParserTemplate, Whole},
    time::{Multiplier, TimeUnit, TimeUnitsLike},
};
use kithara_platform::time::Duration;
use serde::{Deserializer, de};

#[derive(Clone, Copy)]
enum Unit {
    Nanosecond,
    Microsecond,
    Millisecond,
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

struct UnitDef {
    time_unit: TimeUnit,
    multiplier: Multiplier,
    identifiers: &'static [&'static str],
}

mod consts {
    use super::{Config, Multiplier, TimeUnit, UnitDef, whitespace};

    pub(super) const ONE: Multiplier = Multiplier(1, 0);
    pub(super) const MONTH: Multiplier = Multiplier(2_630_016, 0);
    pub(super) const ALL: &[UnitDef] = &[
        UnitDef {
            time_unit: TimeUnit::NanoSecond,
            multiplier: ONE,
            identifiers: &["ns", "nsec", "nanos"],
        },
        UnitDef {
            time_unit: TimeUnit::MicroSecond,
            multiplier: ONE,
            identifiers: &["us", "usec", "µs"],
        },
        UnitDef {
            time_unit: TimeUnit::MilliSecond,
            multiplier: ONE,
            identifiers: &["ms", "msec", "millis"],
        },
        UnitDef {
            time_unit: TimeUnit::Second,
            multiplier: ONE,
            identifiers: &["s", "sec", "secs", "second", "seconds"],
        },
        UnitDef {
            time_unit: TimeUnit::Minute,
            multiplier: ONE,
            identifiers: &["m", "min", "mins", "minute", "minutes"],
        },
        UnitDef {
            time_unit: TimeUnit::Hour,
            multiplier: ONE,
            identifiers: &["h", "hr", "hrs", "hour", "hours"],
        },
        UnitDef {
            time_unit: TimeUnit::Day,
            multiplier: ONE,
            identifiers: &["d", "day", "days"],
        },
        UnitDef {
            time_unit: TimeUnit::Week,
            multiplier: ONE,
            identifiers: &["w", "wk", "wks", "week", "weeks"],
        },
        UnitDef {
            time_unit: TimeUnit::Second,
            multiplier: MONTH,
            identifiers: &["M", "month", "months"],
        },
        UnitDef {
            time_unit: TimeUnit::Year,
            multiplier: ONE,
            identifiers: &["y", "yr", "yrs", "year", "years"],
        },
    ];
    pub(super) const CONFIG: Config<'static> = Config::builder()
        .allow_time_unit_delimiter()
        .parse_multiple(None)
        .outer_delimiter(whitespace)
        .disable_exponent()
        .disable_infinity()
        .build();
}

struct TimeUnits;

impl TimeUnitsLike for TimeUnits {
    fn is_empty(&self) -> bool {
        consts::ALL.is_empty()
    }

    fn get(&self, identifier: &str) -> Option<(TimeUnit, Multiplier)> {
        consts::ALL
            .iter()
            .find(|unit| unit.identifiers.contains(&identifier))
            .map(|unit| (unit.time_unit, unit.multiplier))
    }
}

fn whitespace(byte: u8) -> bool {
    byte.is_ascii_whitespace()
}

#[derive(Clone, Copy)]
enum Scale {
    Seconds(u64),
    Nanos(u64),
}

impl From<Unit> for Scale {
    fn from(unit: Unit) -> Self {
        match unit {
            Unit::Nanosecond => Self::Nanos(1),
            Unit::Microsecond => Self::Nanos(1_000),
            Unit::Millisecond => Self::Nanos(1_000_000),
            Unit::Second => Self::Seconds(1),
            Unit::Minute => Self::Seconds(60),
            Unit::Hour => Self::Seconds(3_600),
            Unit::Day => Self::Seconds(86_400),
            Unit::Week => Self::Seconds(604_800),
            Unit::Month => Self::Seconds(2_630_016),
            Unit::Year => Self::Seconds(31_557_600),
        }
    }
}

impl Scale {
    fn fraction_scale(unit: Unit) -> Result<Self, String> {
        match unit {
            Unit::Nanosecond => Err(String::from(
                "fractional nanoseconds cannot be represented exactly",
            )),
            Unit::Microsecond => Ok(Self::Nanos(1_000)),
            Unit::Millisecond => Ok(Self::Nanos(1_000_000)),
            Unit::Second => Ok(Self::Nanos(1_000_000_000)),
            Unit::Minute => Ok(Self::Nanos(60_000_000_000)),
            Unit::Hour => Ok(Self::Seconds(3_600)),
            Unit::Day => Ok(Self::Seconds(86_400)),
            Unit::Week => Ok(Self::Seconds(604_800)),
            Unit::Month => Ok(Self::Seconds(2_630_016)),
            Unit::Year => Ok(Self::Seconds(31_557_600)),
        }
    }

    fn apply(self, number: u64) -> Result<(u64, u64), String> {
        match self {
            Self::Seconds(scale) => number
                .checked_mul(scale)
                .map(|seconds| (seconds, 0))
                .ok_or_else(number_overflow),
            Self::Nanos(scale) => number
                .checked_mul(scale)
                .map(|nanos| (0, nanos))
                .ok_or_else(number_overflow),
        }
    }
}

fn number_overflow() -> String {
    String::from("number is too large")
}

fn unit_of(repr: &DurationRepr<'_>) -> Option<Unit> {
    match (repr.unit, repr.multiplier) {
        (Some(TimeUnit::NanoSecond), consts::ONE) => Some(Unit::Nanosecond),
        (Some(TimeUnit::MicroSecond), consts::ONE) => Some(Unit::Microsecond),
        (Some(TimeUnit::MilliSecond), consts::ONE) => Some(Unit::Millisecond),
        (Some(TimeUnit::Second), consts::ONE) => Some(Unit::Second),
        (Some(TimeUnit::Minute), consts::ONE) => Some(Unit::Minute),
        (Some(TimeUnit::Hour), consts::ONE) => Some(Unit::Hour),
        (Some(TimeUnit::Day), consts::ONE) => Some(Unit::Day),
        (Some(TimeUnit::Week), consts::ONE) => Some(Unit::Week),
        (Some(TimeUnit::Second), consts::MONTH) => Some(Unit::Month),
        (Some(TimeUnit::Year), consts::ONE) => Some(Unit::Year),
        _ => None,
    }
}

fn whole_number(repr: &DurationRepr<'_>) -> Result<u64, String> {
    let Some(whole) = repr.whole else {
        return Err(number_overflow());
    };
    Whole::parse(&repr.input[whole.0..whole.1], None, None).ok_or_else(number_overflow)
}

fn fraction(fraction: fundu_core::parse::Fract, input: &[u8]) -> Result<(u64, u64), String> {
    let digits = &input[fraction.0..fraction.1];
    let denominator = digits.iter().try_fold(1u64, |value, _| {
        value.checked_mul(10).ok_or_else(number_overflow)
    })?;
    let Some(first_nonzero) = digits.iter().position(|digit| *digit != b'0') else {
        return Ok((0, denominator));
    };
    let numerator =
        Whole::parse(&digits[first_nonzero..], None, None).ok_or_else(number_overflow)?;
    Ok((numerator, denominator))
}

fn exact_division(numerator: u64, denominator: u64) -> Result<u64, String> {
    if numerator.is_multiple_of(denominator) {
        Ok(numerator / denominator)
    } else {
        Err(String::from(
            "fractional duration cannot be represented exactly",
        ))
    }
}

fn add_current(sec: u64, nsec: u64, out: &mut Duration) -> Result<(), String> {
    let mut nsec = u64::from(out.subsec_nanos())
        .checked_add(nsec)
        .ok_or_else(number_overflow)?;
    let mut sec = sec;
    if nsec >= 1_000_000_000 {
        sec = sec
            .checked_add(nsec / 1_000_000_000)
            .ok_or_else(number_overflow)?;
        nsec %= 1_000_000_000;
    }
    let secs = out.as_secs().checked_add(sec).ok_or_else(number_overflow)?;
    let nanos = u32::try_from(nsec).map_err(|_| number_overflow())?;
    *out = Duration::new(secs, nanos);
    Ok(())
}

fn validate_repr(repr: &DurationRepr<'_>) -> Result<(), String> {
    if repr.is_negative.is_some()
        || repr.is_infinite
        || repr.numeral.is_some()
        || repr.exponent != 0
        || repr.unit.is_none()
    {
        return Err(String::from("unsupported duration syntax"));
    }
    if repr.fract.is_some()
        && (repr.whole.is_none() || repr.fract.is_some_and(|fract| fract.is_empty()))
    {
        return Err(String::from("unsupported duration fraction"));
    }
    Ok(())
}

fn unit_and_whole(repr: &DurationRepr<'_>) -> Result<(Unit, u64), String> {
    let unit = unit_of(repr).ok_or_else(|| String::from("unsupported duration unit"))?;
    whole_number(repr).map(|whole| (unit, whole))
}

fn add_whole(unit: Unit, whole: u64, out: &mut Duration) -> Result<(), String> {
    let (sec, nsec) = Scale::from(unit).apply(whole)?;
    add_current(sec, nsec, out)
}

fn scaled_fraction(scale: Scale, numerator: u64, denominator: u64) -> Result<(u64, u64), String> {
    match scale {
        Scale::Seconds(scale) => Ok((
            exact_division(
                numerator.checked_mul(scale).ok_or_else(number_overflow)?,
                denominator,
            )?,
            0,
        )),
        Scale::Nanos(scale) => Ok((
            0,
            exact_division(
                numerator.checked_mul(scale).ok_or_else(number_overflow)?,
                denominator,
            )?,
        )),
    }
}

fn add_fraction(
    unit: Unit,
    fract: fundu_core::parse::Fract,
    input: &[u8],
    out: &mut Duration,
) -> Result<(), String> {
    let (numerator, denominator) = fraction(fract, input)?;
    let (sec, nsec) = scaled_fraction(Scale::fraction_scale(unit)?, numerator, denominator)?;
    add_current(sec, nsec, out)
}

fn add_repr(repr: &DurationRepr<'_>, out: &mut Duration) -> Result<(), String> {
    validate_repr(repr)?;
    let (unit, whole) = unit_and_whole(repr)?;
    add_whole(unit, whole, out)?;
    if let Some(fract) = repr.fract {
        add_fraction(unit, fract, repr.input, out)?;
    }
    Ok(())
}

fn parse(value: &str) -> Result<Duration, String> {
    let trimmed = value.trim_matches(|character: char| character.is_ascii_whitespace());
    if trimmed == "0" {
        if value == "0" {
            return Ok(Duration::ZERO);
        }
        return Err(String::from("unsupported duration syntax"));
    }

    // `fundu`'s high-level duration parser saturates at `Duration::MAX`; keep its
    // published representation parser for syntax and convert each span with the
    // checked arithmetic the previous configuration parser exposed.
    let mut out = Duration::ZERO;
    let mut parser = &mut ReprParserMultiple::new(trimmed);
    loop {
        let (repr, next) = parser
            .parse(&consts::CONFIG, &TimeUnits, None, None)
            .map_err(|error| error.to_string())?;
        add_repr(&repr, &mut out)?;
        match next {
            Some(next_parser) => parser = next_parser,
            None => return Ok(out),
        }
    }
}

struct DurationVisitor;

impl de::Visitor<'_> for DurationVisitor {
    type Value = Duration;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a duration such as 250ms or 2h 37min")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        parse(value).map_err(|_| E::invalid_value(de::Unexpected::Str(value), &self))
    }
}

struct OptionDurationVisitor;

impl<'de> de::Visitor<'de> for OptionDurationVisitor {
    type Value = Option<Duration>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an optional duration such as 250ms")
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserialize(deserializer).map(Some)
    }
}

/// Deserializes a [`Duration`] from the configuration duration syntax.
///
/// # Errors
///
/// Returns an error when the input is not valid configuration duration text.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    deserializer.deserialize_str(DurationVisitor)
}

/// Serde support for optional durations.
pub mod option {
    use serde::Deserializer;

    use super::{Duration, OptionDurationVisitor};

    /// Deserializes an optional [`Duration`] from the configuration duration syntax.
    ///
    /// # Errors
    ///
    /// Returns an error when the present input is not valid configuration duration text.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Duration>, D::Error> {
        deserializer.deserialize_option(OptionDurationVisitor)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    struct Config {
        #[serde(with = "crate::duration")]
        timeout: Duration,
        #[serde(default, deserialize_with = "crate::duration::option::deserialize")]
        idle: Option<Duration>,
    }

    #[kithara::test]
    fn duration_config_values_keep_the_existing_syntax() {
        for (text, expected) in [
            ("0", Duration::ZERO),
            ("32ms", Duration::from_millis(32)),
            ("250us", Duration::from_micros(250)),
            ("45s", Duration::from_secs(45)),
            ("4.2s", Duration::new(4, 200_000_000)),
            ("2h 37min", Duration::from_secs(9_420)),
            ("1month", Duration::from_secs(2_630_016)),
            ("1year", Duration::from_secs(31_557_600)),
            ("17nsec", Duration::from_nanos(17)),
            ("17nanos", Duration::from_nanos(17)),
            ("3usec", Duration::from_nanos(3_000)),
            ("163µs", Duration::from_nanos(163_000)),
            ("31msec", Duration::from_nanos(31_000_000)),
            ("300sec", Duration::from_secs(300)),
            ("100m", Duration::from_secs(6_000)),
            ("7hr", Duration::from_secs(25_200)),
            ("24hours", Duration::from_secs(86_400)),
            ("104wks", Duration::from_secs(62_899_200)),
            ("12M", Duration::from_secs(31_560_192)),
            ("17y", Duration::from_secs(536_479_200)),
            ("3.001us", Duration::from_nanos(3_001)),
            ("31.000001ms", Duration::from_nanos(31_000_001)),
            ("1.000000002s", Duration::from_nanos(1_000_000_002)),
            ("1.123456789s", Duration::from_nanos(1_123_456_789)),
            ("1.0000000000s", Duration::from_secs(1)),
            ("1.0000000010s", Duration::from_nanos(1_000_000_001)),
            ("1.5minute", Duration::from_secs(90)),
            ("2.5h", Duration::from_secs(9_000)),
            ("1.5days", Duration::from_secs(129_600)),
            ("1.234s 1.345ms 1.678us 1ns", Duration::new(1, 235_346_679)),
            ("1.234s0.345ms0.678us0ns", Duration::new(1, 234_345_678)),
            ("20 min 17 nsec ", Duration::new(1_200, 17)),
            (" 2h 15m ", Duration::from_secs(8_100)),
            ("2h 15m", Duration::from_secs(8_100)),
            ("1h30m", Duration::from_secs(5_400)),
            ("1h 30min", Duration::from_secs(5_400)),
        ] {
            assert_eq!(parse(text), Ok(expected), "{text}");
        }
        let config: Config = serde_yaml_ng::from_str("timeout: 250us\nidle: 4.2s\n")
            .expect("the duration config parses");
        assert_eq!(config.timeout, Duration::from_micros(250));
        assert_eq!(config.idle, Some(Duration::new(4, 200_000_000)));
        let config: Config =
            serde_yaml_ng::from_str("timeout: 25ms\n").expect("an absent optional duration parses");
        assert_eq!(config.idle, None);
    }

    #[kithara::test]
    fn duration_overflow_uses_checked_boundary_contract() {
        let max_seconds = u64::MAX;
        let just_before_max = Duration::new(max_seconds - 1, 999_999_999);
        for (text, expected) in [
            ("18446744073709551615s", Duration::new(max_seconds, 0)),
            ("18446744073709551615.0s", Duration::new(max_seconds, 0)),
            ("18446744073709551615s 0ns", Duration::new(max_seconds, 0)),
            (
                "18446744073709551615.000000001s",
                Duration::new(max_seconds, 1),
            ),
            ("18446744073709551615.999999999s", Duration::MAX),
            ("18446744073709551615s 999999999ns", Duration::MAX),
            ("18446744073709551615s999999999ns", Duration::MAX),
            ("18446744073709551615sec999999999nanos", Duration::MAX),
            ("18446744073709551614s 1s 999999999ns", Duration::MAX),
            ("18446744073709551614s1s999999999ns", Duration::MAX),
            ("18446744073709551614.999999999s 1s", Duration::MAX),
            ("18446744073709551614s999999999ns 1s", Duration::MAX),
            ("18446744073709551614.999999999s", just_before_max),
            ("18446744073709551614s999999999ns", just_before_max),
        ] {
            assert_eq!(parse(text), Ok(expected), "{text}");
        }
        let config: Config = serde_yaml_ng::from_str(
            "timeout: 18446744073709551615s\nidle: 18446744073709551615s 999999999ns\n",
        )
        .expect("the maximum duration config parses");
        assert_eq!(config.timeout, Duration::new(max_seconds, 0));
        assert_eq!(config.idle, Some(Duration::MAX));
    }

    #[kithara::test]
    fn invalid_durations_are_rejected() {
        for text in [" 0", "0 "] {
            assert!(parse(text).is_err(), "{text}");
        }
        for text in [
            "",
            "bad",
            "45",
            "0.0",
            "1x",
            "-1s",
            "+1s",
            "1e3s",
            "inf",
            "1.s",
            ".1s",
            "1..s",
            "17.5ns",
            "1.5ns",
            "3.0001us",
            "31.0000001ms",
            "1.0000000002s",
            "0.0000000002s",
            "0.000123456789s",
            "0.00000005d",
            "18446744073709551616s",
            "18446744073709551616.0s",
            "18446744073709551615s 1s",
            "18446744073709551615s1000000000ns",
            "18446744073709551615.999999999s 1ns",
            "18446744073709551615.9999999991s",
            "18446744073709551615.0000000001s",
            "18446744073709551615ms",
            "18446744073709551615.999999999ms",
            "100000000000000000000ns",
            "100000000000000000us",
            "100000000000000ms",
            "100000000000000000000s",
            "10000000000000000000m",
            "1000000000000000000h",
            "100000000000000000d",
            "10000000000000000w",
            "1000000000000000M",
            "10000000000000y",
        ] {
            assert!(parse(text).is_err(), "{text}");
            assert!(
                serde_yaml_ng::from_str::<Config>(&format!("timeout: {text}\n")).is_err(),
                "{text}"
            );
        }
    }
}
