<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-workspace-hack

Internal workspace-hack crate managed by [cargo-hakari](https://docs.rs/cargo-hakari). Not published; not for direct use.

## Why this exists

Hakari produces a unified feature set for third-party dependencies so Cargo unifies them into a single build per dep, avoiding duplicate compilation across workspace members. Every publishable crate in this workspace pulls in `kithara-workspace-hack` as an internal dependency to lock in those unified features.

## Maintenance

`Cargo.toml` and `src/lib.rs` are auto-generated. Do not edit manually. To regenerate:

```bash
cargo install cargo-hakari --locked
cargo hakari generate
cargo hakari manage-deps
```

Run after adding or removing workspace dependencies, or when CI flags hakari drift.

## Configuration

The hakari config lives in `.config/hakari.toml` (or workspace root if present). It controls platform/feature combinations and which workspace crates participate.
