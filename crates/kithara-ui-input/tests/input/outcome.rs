use kithara_test_utils::kithara;
use kithara_ui_input::{Outcome, PointerOwnership, Propagation};

#[kithara::test]
fn propagation_and_retained_pointer_ownership_are_independent() {
    let claim = Outcome::<()>::captured().with_ownership(PointerOwnership::Claim);
    assert_eq!(claim.propagation(), Propagation::Captured);
    assert_eq!(claim.ownership(), PointerOwnership::Claim);

    let release = Outcome::observed(42).with_ownership(PointerOwnership::Release);
    assert_eq!(release.propagation(), Propagation::Ignored);
    assert_eq!(release.ownership(), PointerOwnership::Release);
    assert_eq!(release.value(), Some(42));
}

#[kithara::test]
fn existing_outcome_constructors_leave_pointer_ownership_unchanged() {
    assert_eq!(
        Outcome::<()>::IGNORED.ownership(),
        PointerOwnership::Unchanged
    );
    assert_eq!(
        Outcome::<()>::captured().ownership(),
        PointerOwnership::Unchanged
    );
    assert_eq!(Outcome::set(3).ownership(), PointerOwnership::Unchanged);
    assert_eq!(
        Outcome::observed(3).ownership(),
        PointerOwnership::Unchanged
    );
}
