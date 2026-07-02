# Agent Hooks

Use this only when touching tool adapters, hook behavior, or command routing.
Hooks are workflow guards, not code-style policy. Architecture and Rust shape are
still enforced by `ast-grep` and `cargo xtask lint`.

## Pre Bash Guard

`cargo xtask agent-hook pre-bash` reads Claude hook JSON from stdin and denies
only common expensive command mistakes:

- Broad raw test acceptance: `cargo test`, `cargo test --workspace`, or
  `cargo nextest run --workspace` without a package/filter. Use
  `cargo xtask test` or `just test`.
- Formatter bypasses: direct `rustfmt`, `cargo sort --check`, `taplo format`,
  or `mdfmt` as a gate. Use `cargo xtask format`.
- Outer timeouts around the full harness: `timeout just test` or
  `timeout cargo xtask test`.
- Destructive git commands such as `git reset --hard`, `git clean`, or
  `git checkout -- ...`. The only override marker is
  `KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT=1`, and it requires explicit user
  approval first.

Scoped probes are allowed. Examples: `cargo test -p xtask agent_hook`,
`cargo nextest run -p kithara-platform -E 'test(foo)'`, or a workspace nextest
run with a filter expression.

## Post Edit Format

`cargo xtask agent-hook post-edit` formats only known edited file classes:

- `.rs` -> `cargo xtask format --only rust --allow-dirty`
- `Cargo.toml` -> `cargo xtask format --only manifest --allow-dirty`
- other `.toml` -> `cargo xtask format --only toml --allow-dirty`
- `.json` / `.jsonc` -> `cargo xtask format --only json --allow-dirty`

It does not run tests, lints, markdown formatting, or architecture checks.

## Tool Adapter Rule

Tool-specific files such as `.claude/settings.json` should only call repo-owned
commands or route to canonical docs. Do not duplicate policy text there.
