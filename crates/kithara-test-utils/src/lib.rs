// `unsafe_code` is permitted only in `probes::usdt_wire` to host the
// inline-asm USDT provider expansion; the rest of this crate stays
// `unsafe`-free.
#![allow(
    clippy::unwrap_used,
    reason = "test utility crate — unwraps are acceptable"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    reason = "test utility crate — numeric casts are acceptable for WAV generation"
)]
#![allow(
    clippy::missing_panics_doc,
    reason = "test utility crate — panic documentation not needed"
)]

//! Shared test utilities for the kithara workspace.
//!
//! Прод-сборка видит только лёгкий `probes/` runtime (trait `Probe`,
//! `IntoProbeArg`, no-op `fire_N` stubs) и `pub mod kithara` re-export
//! макросов. Всё остальное — фикстуры, тестовый HTTP-сервер, fmp4,
//! signal/wav-генераторы — за единственным `feature = "test-utils"`,
//! который dev-deps потребителей включают для тестовой сборки.

#[cfg(test)]
extern crate self as kithara_test_utils;

pub mod probes;

/// Re-export of `kithara_test_macros` под `kithara::*`-namespace.
/// Позволяет писать `use kithara_test_utils::kithara` и затем
/// `#[kithara::test]`, `#[kithara::probe]`, `#[kithara::mock]`.
pub mod kithara {
    pub use kithara_test_macros::{Probe, fixture, mock, probe, test};
}

#[cfg(feature = "test-utils")]
mod inner;

#[cfg(feature = "test-utils")]
pub use inner::*;
