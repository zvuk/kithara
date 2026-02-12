# Contributing to kithara

Thank you for your interest in contributing! Whether it's a bug report, feature request, or code contribution — all help is welcome.

## Getting Started

```bash
# Fork and clone
git clone https://github.com/<your-fork>/kithara.git
cd kithara

# Build
cargo build --workspace

# Run tests
cargo test --workspace

# Check formatting and lints
cargo fmt --all --check
cargo clippy --workspace -- -D warnings

# Run style linter
bash scripts/lint-style.sh
```

**System dependencies:**
- Linux: `libasound2-dev` (ALSA headers)
- macOS: Xcode Command Line Tools (for AudioToolbox)

## Development Workflow

### Branching

Create a feature branch from `main`:

```bash
git checkout -b feat/short-description
```

Use prefixes: `feat/`, `fix/`, `refactor/`, `docs/`, `test/`.

### Commits

Write clear, concise commit messages. Use conventional style:

```
feat(hls): add segment prefetch for faster start
fix(decode): handle zero-length packets without panic
docs: update kithara-net README
```

### Test-Driven Development

We follow TDD:

1. Write a failing test that describes the desired behavior.
2. Run the test — confirm it fails for the expected reason.
3. Implement the minimum code to make the test pass.
4. Refactor only after tests are green.

## Pull Request Process

1. Ensure all checks pass locally:
   - `cargo fmt --all --check`
   - `cargo clippy --workspace -- -D warnings`
   - `cargo test --workspace`
   - `bash scripts/lint-style.sh`

2. Open a PR against `main` with a clear description.

3. Fill in the PR template checklist.

4. One approval from a maintainer is required before merge.

### What we look for in review

- Tests for new behavior
- No `unwrap()` / `expect()` in production code
- Logging via `tracing`, never `println!` / `dbg!`
- Typed errors via `thiserror`
- No buffer allocations on hot paths — use `SharedPool` from `kithara-bufpool`
- Workspace-first dependencies (versions only in root `Cargo.toml`)

See [`AGENTS.md`](AGENTS.md) for the full set of coding rules.

## Architecture Overview

kithara is a modular workspace of 12 crates. The dependency graph and crate roles are documented in the [root README](README.md#crate-architecture).

Key layers (top to bottom):
- **kithara** — facade with auto-detection (file vs HLS)
- **kithara-audio** — audio pipeline, resampling, effects
- **kithara-decode** — Symphonia (software) and Apple AudioToolbox (hardware) decoders
- **kithara-file / kithara-hls** — protocol-specific streaming
- **kithara-stream** — async-to-sync byte bridge (`Read + Seek`)
- **kithara-storage / kithara-assets** — mmap storage and disk cache

Each crate has its own `README.md` documenting its public contract.

## First-Time Contributors

Look for issues labeled [`good first issue`](https://github.com/zvuk/kithara/labels/good%20first%20issue). These are scoped tasks suitable for newcomers.

Good areas to start:
- Improving documentation or examples
- Adding tests for untested code paths (see `docs/test-coverage-gaps.md`)
- Small bug fixes with clear reproduction steps

## Developer Certificate of Origin (DCO)

By contributing, you certify that you wrote the code (or have the right to submit it) under the project's license. We use the [Developer Certificate of Origin](https://developercertificate.org/).

Sign off your commits:

```bash
git commit -s -m "feat(net): add request timeout configuration"
```

This adds a `Signed-off-by: Your Name <your@email.com>` line. All commits in a PR must be signed off.

## Code of Conduct

This project follows the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md). By participating, you agree to uphold a welcoming, inclusive environment.

## License

By contributing, you agree that your contributions will be licensed under the same dual license as the project: [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).
