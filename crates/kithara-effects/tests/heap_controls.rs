#![forbid(unsafe_code)]

use kithara_test_utils::{
    kithara,
    memory::{self, Counting},
};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[kithara::test(native)]
fn held_drop_and_reset_use_the_owner_accounting_boundaries() {
    let before = memory::live_bytes();
    let (held, observation) = memory::measure(|| vec![0_u8; 4096]);
    assert_eq!(observation.held, held.len());
    assert!(observation.peak >= observation.held);
    assert_eq!(memory::live_bytes(), before + held.len());
    drop(held);
    assert_eq!(memory::live_bytes(), before);
    memory::reset_peak();
    assert_eq!(memory::peak_bytes(), memory::live_bytes());
}
