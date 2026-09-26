use std::collections::BTreeSet;

use kithara_test_utils::kithara;
use kithara_ui::expand::ControlSpec;

#[kithara::test]
fn no_control_is_named_twice() {
    let unique: BTreeSet<&str> = ControlSpec::KINDS.iter().copied().collect();
    assert_eq!(
        unique.len(),
        ControlSpec::KINDS.len(),
        "a duplicate name would let one control stand in for another"
    );
}

#[kithara::test]
fn a_value_reports_the_name_the_set_lists() {
    assert_eq!(ControlSpec::Spacer.kind(), "Spacer");
    assert!(ControlSpec::KINDS.contains(&ControlSpec::WindowDrag.kind()));
}
