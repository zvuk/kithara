<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-ffi

Cross-platform FFI adapter for the kithara audio player. Not published — consumed by Apple (Swift via UniFFI) and Android (Kotlin via UniFFI / JNI) build flows.

## Role

- Exposes a stable, language-agnostic surface over `kithara-play` so platform shims (`kithara/apple`, `kithara/android`) can talk to the engine without depending on internal Rust types.
- Owns the UniFFI definitions used by `cargo xtask apple xcframework` and `cargo xtask android aar`.
- On Android, `src/android_test.rs` is gated on `target_os = "android"` + `feature = "test"` and is the only consumer of `kithara-play` from this crate.

## Layout

- `src/` — FFI bindings, UniFFI types, error mapping.
- `uniffi.toml` — UniFFI configuration consumed by the xtask build flows.

## Build flows

See the workspace tooling for end-to-end builds:

- `just apple xcframework` — builds the Apple XCFramework (release).
- `just android aar` — builds Android AARs (release).

This crate is `publish = false`. Do not depend on it directly from application code — use the platform-specific shims (`kithara/apple`, `kithara/android`).
