#[cfg(all(not(target_arch = "wasm32"), feature = "test-utils"))]
#[allow(
    clippy::undocumented_unsafe_blocks,
    reason = "Inline-asm `unsafe` blocks expanded by `usdt::provider!` are out of our control."
)]
#[::usdt::provider(provider = "kithara")]
mod prov {
    pub(super) fn probe_0() {}
    pub(super) fn probe_1(_a0: u64) {}
    pub(super) fn probe_2(_a0: u64, _a1: u64) {}
    pub(super) fn probe_3(_a0: u64, _a1: u64, _a2: u64) {}
    pub(super) fn probe_4(_a0: u64, _a1: u64, _a2: u64, _a3: u64) {}
    pub(super) fn probe_5(_a0: u64, _a1: u64, _a2: u64, _a3: u64, _a4: u64) {}
    pub(super) fn probe_6(_a0: u64, _a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64) {}
}

#[cfg_attr(
    feature = "test-utils",
    expect(
        clippy::items_after_statements,
        reason = "USDT probe macro mixes per-call `static` items with `let _: u64 = ...` arg-bindings; the shape is fixed by the external `usdt` crate."
    )
)]
pub fn fire_0(name: &'static str) {
    let _ = name;
    #[cfg(all(not(target_arch = "wasm32"), feature = "test-utils"))]
    prov::probe_0!(|| ());
}

#[cfg_attr(
    feature = "test-utils",
    expect(
        clippy::items_after_statements,
        reason = "USDT probe macro mixes per-call `static` items with `let _: u64 = ...` arg-bindings; the shape is fixed by the external `usdt` crate."
    )
)]
pub fn fire_1(name: &'static str, a0: u64) {
    let _ = (name, a0);
    #[cfg(all(not(target_arch = "wasm32"), feature = "test-utils"))]
    prov::probe_1!(|| a0);
}

#[cfg_attr(
    feature = "test-utils",
    expect(
        clippy::items_after_statements,
        reason = "USDT probe macro mixes per-call `static` items with `let _: u64 = ...` arg-bindings; the shape is fixed by the external `usdt` crate."
    )
)]
pub fn fire_2(name: &'static str, a0: u64, a1: u64) {
    let _ = (name, a0, a1);
    #[cfg(all(not(target_arch = "wasm32"), feature = "test-utils"))]
    prov::probe_2!(|| (a0, a1));
}

#[cfg_attr(
    feature = "test-utils",
    expect(
        clippy::items_after_statements,
        reason = "USDT probe macro mixes per-call `static` items with `let _: u64 = ...` arg-bindings; the shape is fixed by the external `usdt` crate."
    )
)]
pub fn fire_3(name: &'static str, a0: u64, a1: u64, a2: u64) {
    let _ = (name, a0, a1, a2);
    #[cfg(all(not(target_arch = "wasm32"), feature = "test-utils"))]
    prov::probe_3!(|| (a0, a1, a2));
}

#[cfg_attr(
    feature = "test-utils",
    expect(
        clippy::items_after_statements,
        reason = "USDT probe macro mixes per-call `static` items with `let _: u64 = ...` arg-bindings; the shape is fixed by the external `usdt` crate."
    )
)]
pub fn fire_4(name: &'static str, a0: u64, a1: u64, a2: u64, a3: u64) {
    let _ = (name, a0, a1, a2, a3);
    #[cfg(all(not(target_arch = "wasm32"), feature = "test-utils"))]
    prov::probe_4!(|| (a0, a1, a2, a3));
}

#[cfg_attr(
    feature = "test-utils",
    expect(
        clippy::items_after_statements,
        reason = "USDT probe macro mixes per-call `static` items with `let _: u64 = ...` arg-bindings; the shape is fixed by the external `usdt` crate."
    )
)]
pub fn fire_5(name: &'static str, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64) {
    let _ = (name, a0, a1, a2, a3, a4);
    #[cfg(all(not(target_arch = "wasm32"), feature = "test-utils"))]
    prov::probe_5!(|| (a0, a1, a2, a3, a4));
}

#[cfg_attr(
    feature = "test-utils",
    expect(
        clippy::items_after_statements,
        reason = "USDT probe macro mixes per-call `static` items with `let _: u64 = ...` arg-bindings; the shape is fixed by the external `usdt` crate."
    )
)]
pub fn fire_6(name: &'static str, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) {
    let _ = (name, a0, a1, a2, a3, a4, a5);
    #[cfg(all(not(target_arch = "wasm32"), feature = "test-utils"))]
    prov::probe_6!(|| (a0, a1, a2, a3, a4, a5));
}
