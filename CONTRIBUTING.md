# Contributing to kithara

Bug reports, feature requests, and code contributions are welcome.

## Quick Start

```bash
git clone https://github.com/<your-fork>/kithara.git
cd kithara
cargo install just --locked
cargo install cargo-nextest --locked
cargo build --workspace
just test-all
```

**System dependencies:** Linux needs `libasound2-dev`, macOS needs Xcode Command Line Tools.

## Before Submitting a PR

```bash
just lint-full
just test-all
```

Recommended local setup:

```bash
cargo install just --locked
cargo install cargo-nextest --locked
# Install prek:
# https://github.com/j178/prek
prek install -f

# Optional: worktree workflow for parallel agents
brew install worktrunk
wt config shell install
```

See [`AGENTS.md`](AGENTS.md) for the full coding rules.

## Architecture

12 modular crates — see the [root README](README.md#crate-architecture) for the dependency graph. Each crate has its own `README.md`.

## License

Contributions are licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).
