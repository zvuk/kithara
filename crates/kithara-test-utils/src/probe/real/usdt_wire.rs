#[cfg(all(target_os = "macos", feature = "usdt", not(miri)))]
#[allow(
    clippy::undocumented_unsafe_blocks,
    reason = "Inline-asm `unsafe` blocks expanded by `usdt::provider!` are out of our control."
)]
#[::usdt::provider(provider = "kithara")]
mod prov {
    pub(super) fn probe_0(_operation: u64) {}
    pub(super) fn probe_1(_operation: u64, _a0: u64) {}
    pub(super) fn probe_2(_operation: u64, _a0: u64, _a1: u64) {}
    pub(super) fn probe_3(_operation: u64, _a0: u64, _a1: u64, _a2: u64) {}
    pub(super) fn probe_4(_operation: u64, _a0: u64, _a1: u64, _a2: u64, _a3: u64) {}
    pub(super) fn probe_5(_operation: u64, _a0: u64, _a1: u64, _a2: u64, _a3: u64, _a4: u64) {}
}

pub fn fire_0(operation: u64) {
    let _ = operation;
    #[cfg(all(target_os = "macos", feature = "usdt", not(miri)))]
    prov::probe_0!(|| operation);
}

#[cfg_attr(
    feature = "usdt",
    expect(
        clippy::cast_possible_truncation,
        clippy::items_after_statements,
        reason = "USDT probe macros from the external `usdt` crate emit `u64 as usize` casts and per-call `static` items in the same scope; both shapes are fixed by upstream."
    )
)]
pub fn fire_1(operation: u64, a0: u64) {
    let _ = (operation, a0);
    #[cfg(all(target_os = "macos", feature = "usdt", not(miri)))]
    prov::probe_1!(|| (operation, a0));
}

#[cfg_attr(
    feature = "usdt",
    expect(
        clippy::cast_possible_truncation,
        clippy::items_after_statements,
        reason = "USDT probe macros from the external `usdt` crate emit `u64 as usize` casts and per-call `static` items in the same scope; both shapes are fixed by upstream."
    )
)]
pub fn fire_2(operation: u64, a0: u64, a1: u64) {
    let _ = (operation, a0, a1);
    #[cfg(all(target_os = "macos", feature = "usdt", not(miri)))]
    prov::probe_2!(|| (operation, a0, a1));
}

#[cfg_attr(
    feature = "usdt",
    expect(
        clippy::cast_possible_truncation,
        clippy::items_after_statements,
        reason = "USDT probe macros from the external `usdt` crate emit `u64 as usize` casts and per-call `static` items in the same scope; both shapes are fixed by upstream."
    )
)]
pub fn fire_3(operation: u64, a0: u64, a1: u64, a2: u64) {
    let _ = (operation, a0, a1, a2);
    #[cfg(all(target_os = "macos", feature = "usdt", not(miri)))]
    prov::probe_3!(|| (operation, a0, a1, a2));
}

#[cfg_attr(
    feature = "usdt",
    expect(
        clippy::cast_possible_truncation,
        clippy::items_after_statements,
        reason = "USDT probe macros from the external `usdt` crate emit `u64 as usize` casts and per-call `static` items in the same scope; both shapes are fixed by upstream."
    )
)]
pub fn fire_4(operation: u64, a0: u64, a1: u64, a2: u64, a3: u64) {
    let _ = (operation, a0, a1, a2, a3);
    #[cfg(all(target_os = "macos", feature = "usdt", not(miri)))]
    prov::probe_4!(|| (operation, a0, a1, a2, a3));
}

#[cfg_attr(
    feature = "usdt",
    expect(
        clippy::cast_possible_truncation,
        clippy::items_after_statements,
        reason = "USDT probe macros from the external `usdt` crate emit `u64 as usize` casts and per-call `static` items in the same scope; both shapes are fixed by upstream."
    )
)]
pub fn fire_5(operation: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64) {
    let _ = (operation, a0, a1, a2, a3, a4);
    #[cfg(all(target_os = "macos", feature = "usdt", not(miri)))]
    prov::probe_5!(|| (operation, a0, a1, a2, a3, a4));
}
