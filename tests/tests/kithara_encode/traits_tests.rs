use kithara_encode::InnerEncoder;
use kithara_test_utils::kithara;

#[kithara::test]
fn trait_is_object_safe() {
    fn _accepts_boxed(_: Box<dyn InnerEncoder>) {}
}
