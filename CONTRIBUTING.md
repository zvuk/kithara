# Contributing to kithara

Thank you for your interest in contributing to kithara! This guide will help you get started.

## Quick Start

```bash
# Build the workspace
cargo build --workspace

# Run all tests
cargo test --workspace

# Check formatting
cargo fmt --all --check

# Run clippy
cargo clippy --workspace -- -D warnings
```

## Pull Request Checklist

Before submitting a PR, ensure:

- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --workspace -- -D warnings` passes with no warnings
- [ ] New code has tests
- [ ] All tests pass (`cargo test --workspace`)
- [ ] No `unwrap()` or `expect()` in production code
- [ ] Logging uses `tracing`, not `println!` or `dbg!`
- [ ] Errors are typed (`thiserror`), not stringly-typed

## Coding Rules

- **No `unwrap()`/`expect()`** in production code. Use proper error handling.
- **Use `tracing`** for logging. Never `println!` or `dbg!`.
- **Typed errors** via `thiserror` in every crate.
- **No buffer allocations** on hot paths. Use `SharedPool` from `kithara-bufpool`.
- **TDD workflow**: write a failing test first, then implement.
- **Workspace-first dependencies**: all versions in root `Cargo.toml`, crates use `{ workspace = true }`.
- **Single source of truth**: shared types (`AudioCodec`, `ContainerFormat`, `MediaInfo`) live in `kithara-stream`.

## Architecture

See the [root README](README.md) for the crate dependency graph and architecture overview. Each crate has its own `README.md` with detailed documentation.

## Running Specific Tests

```bash
# Test a single crate
cargo test -p kithara-hls

# Test a specific function
cargo test -p kithara-hls test_function_name

# Run integration tests
cargo test -p kithara-hls --test playlist_integration
```

## Code of Conduct

This project follows the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md). By participating, you are expected to uphold this code.
