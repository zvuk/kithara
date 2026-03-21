# Contributing to kithara

Bug reports, feature requests, and code contributions are welcome.

## Quick Start

```bash
git clone https://github.com/<your-fork>/kithara.git
cd kithara
cargo install just --locked
cargo build --workspace
just test-all
```

Complete the environment setup from [README.md#environment-setup](README.md#environment-setup) before relying on the `just` workflow.

The canonical local workflow for both humans and AI agents lives in [.docs/workflow/rust-ai.md](.docs/workflow/rust-ai.md).

## Before Submitting a PR

```bash
just lint-full
just test-all
```

Run extra checks when the change affects multiple crates, public APIs, wasm, perf-sensitive code, or workflow tooling. Use the validation scope from your task packet or plan together with the touched crate `README.md` files to choose the right local commands.

Recommended local setup:

```bash
cargo install just --locked

# Optional: worktree helper for parallel agents
brew install worktrunk
wt config shell install
```

See [`AGENTS.md`](AGENTS.md) for the full coding rules and [.docs/plans/_template.md](.docs/plans/_template.md) for the required task-plan format on non-trivial changes.

## Architecture

12 modular crates — see the [root README](README.md#architecture) for the dependency graph. Each crate has its own `README.md`.

## License

Contributions are licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).
