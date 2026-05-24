# xtask

Workspace automation binary for Kithara. Provides repo-local lints and audits that go beyond what `cargo clippy` covers.

## Subcommands

- `cargo run -p xtask -- lint arch` — architectural checks (file size, god structs, fan-out, layering direction, etc.). 33 rules in `.config/arch/thresholds.toml`.
- `cargo run -p xtask -- lint style` — style checks (comment hygiene, struct field/init/trait item ordering, const locality). 5 rules in `.config/style/thresholds.toml`.
- `cargo run -p xtask -- lint idioms` — Rust idiom checks (function branch density, loop allocation, guard cascades, etc.). 18 rules in `.config/idioms/thresholds.toml`.
- `cargo run -p xtask -- ast-grep` — runs the 55 ast-grep rules from `.config/ast-grep/`.

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

Each check is one file under `src/<ns>/checks/<rule>.rs` implementing the `Check` trait.
