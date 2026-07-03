use kithara;
use kithara_encode::InnerEncoder;

#[kithara::test]
fn trait_is_object_safe() {
    fn _accepts_boxed(_: Box<dyn InnerEncoder>) {}
}
