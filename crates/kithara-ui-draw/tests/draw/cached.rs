use kithara_test_utils::kithara;
use kithara_ui_draw::CachedValue;

#[kithara::test]
fn a_fresh_cache_holds_no_value() {
    let cached = CachedValue::<u8, &str>::default();

    assert_eq!(cached.value(), None, "nothing was ever built");
}

#[kithara::test]
fn a_new_key_takes_the_value_that_came_with_it() {
    let mut cached = CachedValue::default();

    cached.update(1_u8, Some("one"));

    assert_eq!(cached.value(), Some(&"one"));
}

#[kithara::test]
fn the_key_moves_with_the_value_it_admitted() {
    let mut cached = CachedValue::default();

    cached.update(1_u8, Some("one"));

    assert_eq!(cached.key(), &1, "the value is only as good as its key");
}

#[kithara::test]
fn a_key_that_still_holds_keeps_its_value() {
    let mut cached = CachedValue::default();
    cached.update(1_u8, Some("one"));

    cached.update(1, None);

    assert_eq!(cached.value(), Some(&"one"), "the key did not move");
}

#[kithara::test]
fn a_moved_key_takes_the_value_offered_with_it() {
    let mut cached = CachedValue::default();
    cached.update(1_u8, Some("one"));

    cached.update(2, Some("two"));

    assert_eq!(cached.value(), Some(&"two"));
}
