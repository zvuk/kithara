//! Behaviour of the code `#[derive(Ranged)]` emits.

use kithara_derive::Ranged;
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_utils::kithara;

/// Asymmetric on purpose: the two ends have to be read separately.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = -24.0, max = 6.0, default = 0.0, clamp)]
struct Probe(f32);

/// A document value: no `clamp`, so every door refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Ranged)]
#[ranged(min = 0, max = 100, default = 100)]
struct Share(u8);

#[derive(Debug, serde::Deserialize)]
struct Doc {
    max_share: Share,
}

#[derive(Debug, serde::Deserialize)]
struct GainDoc {
    gain: Probe,
}

#[kithara::test]
#[case::above_the_ceiling(100.0, Probe::MAX)]
#[case::below_the_floor(-100.0, Probe::MIN)]
#[case::not_a_number(f32::NAN, Probe::DEFAULT)]
fn clamping_uses_the_declared_bound_or_default(#[case] value: f32, #[case] expected: Probe) {
    assert_eq!(Probe::from(value), expected);
}

#[kithara::test]
fn a_value_inside_the_range_is_kept_exactly() {
    assert_eq!(f32::from(Probe::from(-3.5)), -3.5);
}

#[kithara::test]
fn the_default_value_is_the_declared_one() {
    assert_eq!(Probe::default(), Probe::DEFAULT);
}

#[kithara::test]
#[case::inside_the_range(-3.5, Some(Probe::from(-3.5)))]
#[case::below_the_floor(-24.1, None)]
#[case::above_the_ceiling(6.1, None)]
#[case::not_a_number(f32::NAN, None)]
#[case::positive_infinity(f32::INFINITY, None)]
#[case::negative_infinity(f32::NEG_INFINITY, None)]
fn checked_construction_enforces_the_float_range(
    #[case] value: f32,
    #[case] expected: Option<Probe>,
) {
    assert_eq!(Probe::checked(value), expected);
}

#[kithara::test]
#[case::at_the_ceiling(100, Some(Share::MAX))]
#[case::above_the_ceiling(101, None)]
fn checked_construction_enforces_the_integer_range(
    #[case] value: u8,
    #[case] expected: Option<Share>,
) {
    assert_eq!(Share::checked(value), expected);
}

#[kithara::test]
fn an_integer_unwraps_to_its_primitive() {
    assert_eq!(u8::from(Share::MAX), 100);
}

#[kithara::test]
fn an_integer_default_is_the_declared_one() {
    assert_eq!(Share::default(), Share::MAX);
}

#[kithara::test]
fn a_document_value_inside_the_range_parses() {
    let doc: Doc = serde_yaml_ng::from_str("max_share: 100\n").expect("100 is inside the range");

    assert_eq!(doc.max_share, Share::MAX);
}

#[kithara::test]
fn a_document_value_outside_the_range_is_refused_by_name_and_by_bounds() {
    let error =
        serde_yaml_ng::from_str::<Doc>("max_share: 140\n").expect_err("140 is outside the range");
    let message = error.to_string();

    assert!(
        message.contains("max_share"),
        "the field is named: {message}"
    );
    assert!(
        message.contains("140"),
        "the offending value is named: {message}"
    );
    assert!(message.contains('0'), "the floor is named: {message}");
    assert!(message.contains("100"), "the ceiling is named: {message}");
}

/// A knob clamps in Rust. A document never does — this is the one place the
/// declarative macro would have let a `NaN` through silently.
#[kithara::test]
fn a_document_never_clamps_even_for_a_clamping_type() {
    let doc: GainDoc = serde_yaml_ng::from_str("gain: 0.0\n").expect("unity is inside the range");
    assert_eq!(doc.gain, Probe::DEFAULT);
    let error =
        serde_yaml_ng::from_str::<GainDoc>("gain: .nan\n").expect_err("a document refuses a NaN");

    assert!(
        error.to_string().contains("Probe"),
        "the type is named: {error}"
    );
}
