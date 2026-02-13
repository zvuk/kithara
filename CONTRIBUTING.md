# Contributing to kithara

Bug reports, feature requests, and code contributions are welcome.

## Quick Start

```bash
git clone https://github.com/<your-fork>/kithara.git
cd kithara
cargo build --workspace
cargo test --workspace
```

**System dependencies:** Linux needs `libasound2-dev`, macOS needs Xcode Command Line Tools.

## Before Submitting a PR

```bash
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

See [`AGENTS.md`](AGENTS.md) for the full coding rules.

## Architecture

12 modular crates — see the [root README](README.md#crate-architecture) for the dependency graph. Each crate has its own `README.md`.

## License

Contributions are licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).
