# xtask

Workspace automation binary for Kithara. Provides repo-local lints and audits that go beyond what `cargo clippy` covers.

## Subcommands

- `cargo run -p xtask -- lint arch` — architectural checks (file size, god structs, fan-out, layering direction, etc.). 33 rules in `.config/arch/thresholds.toml`.
- `cargo run -p xtask -- lint style` — style checks (comment hygiene, struct field/init/trait item ordering, const locality). 5 rules in `.config/style/thresholds.toml`.
- `cargo run -p xtask -- lint idioms` — Rust idiom checks (function branch density, loop allocation, guard cascades, etc.). 18 rules in `.config/idioms/thresholds.toml`.
- `cargo run -p xtask -- ast-grep` — runs the 55 ast-grep rules from `.config/ast-grep/`.
- `cargo run -p xtask -- format --check` — formatter harness for Rust, Cargo manifests, non-Cargo TOML, and JSON/JSONC.
- `cargo xtask agent-hook install` - atomically installs the current xtask binary and fingerprint in the concrete worktree Git directory and publishes the checkout-local pointer.
- `just --quiet _agent-hook pre-bash|post-edit` - runs the installed command guards through the hidden Just launcher without Cargo or Git-directory discovery.
- `cargo xtask agent-hook pre-bash|post-edit` - explicit diagnostic entry point that bypasses the installed cache.
- `cargo run -p xtask -- manifest dependency-order` — checks that internal `kithara` / `kithara-*` dependencies come before external crates in Cargo manifests.
- `cargo run -p xtask -- health` — broad local health report, including formatter, dependency, unsafe-inventory, lint, quality, and test stages.

## Baselines

Each `lint <namespace>` reads `.config/<namespace>/baseline.toml` and ratchets on regression only. To shrink a baseline:

1. Fix the underlying code.
2. Re-run the linter — it prints the surplus.
3. Drop the entry from `baseline.toml`.

Never grow a baseline to make a commit pass. See `AGENTS.md` Non-Negotiables.

## Layout

- `src/arch/` — architectural checks.
- `src/style/` — style checks.
- `src/idioms/` — idiom checks.
- `src/common/` — shared scope/violation/baseline plumbing.
- `src/agent_hook/` — agent command guards, installation, and cache freshness.

Each check is one file under `src/<ns>/checks/<rule>.rs` implementing the `Check` trait.
