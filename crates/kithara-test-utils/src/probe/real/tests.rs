use super::{fire_0, fire_1, fire_2, fire_3, fire_4, fire_5, register_probes};
use crate::kithara;

#[kithara::test(native, platform = apple)]
fn usdt_registration_and_firing_are_callable() {
    register_probes();
    register_probes();
    fire_0(0);
    fire_1(1, 1);
    fire_2(2, 1, 2);
    fire_3(3, 1, 2, 3);
    fire_4(4, 1, 2, 3, 4);
    fire_5(5, 1, 2, 3, 4, 5);
}
