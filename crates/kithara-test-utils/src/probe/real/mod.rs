#![cfg_attr(target_arch = "wasm32", allow(unused_imports))]

/// DTrace is the native USDT backend on macOS. Other targets use the tracing
/// backend emitted by the probe macro.
#[cfg(all(target_os = "macos", not(miri)))]
mod usdt_wire;
mod wire;

#[cfg(all(target_os = "macos", not(miri)))]
pub use usdt_wire::{fire_0, fire_1, fire_2, fire_3, fire_4, fire_5};
pub use wire::{IntoProbeArg, Probe, operation_id, register_probes};

#[cfg(all(test, target_os = "macos", not(miri)))]
mod tests {
    use super::{fire_0, fire_1, fire_2, fire_3, fire_4, fire_5, register_probes};

    #[test]
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
}
