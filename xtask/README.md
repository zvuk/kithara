# xtask

Workspace automation binary for Kithara. It provides repo-local formatting,
lint, test, audit, release, and agent-hook commands that go beyond raw Cargo
commands.

## Running xtask

Use the generic Just entry point:

```sh
just xtask <subcommand> [args...]
```

All higher-level Just recipes that need xtask delegate to this entry point. It
uses a worktree-local self-cache: a warm invocation runs an immutable cached
binary directly, a stale invocation refreshes it once, and a missing cache uses
Cargo for the cold bootstrap. Force a rebuild after an undeclared environment
change with `just xtask-refresh`.

The ignored `xtask/.xtask-cache` file locates the active generation owned by
the concrete worktree. Cached commands do not use another worktree's binary or
the shared `target/debug/xtask`. Cache policy is configured under
`[ext.xtask.cache]` in `.config/xtask.toml`.

Refresh builds are supervised and fail closed. Cargo returns an artifact but
does not publish it; the parent verifies that declared inputs stayed unchanged
throughout the build before atomically activating the new generation.

## Common Commands

- `just lint` runs the architecture, style, and Rust-idiom checks.
- `just ast-grep` runs the rules under `.config/ast-grep/`.
- `just fmt` formats Rust, Cargo manifests, non-Cargo TOML, and JSON/JSONC.
- `just fmt-check` checks those formatters without rewriting files.
- `just test` runs the repository test harness.
- `just xtask health` runs the broad local health report.

Use `just xtask --help` and `just xtask <subcommand> --help` for the complete
CLI. Direct `cargo run -p xtask` is a scoped development probe, not the normal
repository entry point.

## Agent Hooks

`agent-hook` consumes the tool payload from stdin and derives the event, tool
kind, and edited paths from that payload. Typed routes and the destructive-Git
override variable live under `[ext.agent_hook]` in `.config/xtask.toml`; the
handlers own the guard and formatting policy.

Tool adapters call the `just agent-hook` recipe. That recipe reuses the generic
cache in optional mode, so hooks never invoke Cargo, refresh the binary, or
wait for a build. There are no agent-hook
install/uninstall/status/cache commands and no `pre-bash` or `post-edit` CLI
subcommands. See `docs/guides/agent-hooks.md` for the routing and failure
contract.

## Baselines

Each `lint <namespace>` reads `.config/<namespace>/baseline.toml` and ratchets
on regression only. To shrink a baseline:

1. Fix the underlying code.
1. Re-run the linter; it prints the surplus.
1. Drop the entry from `baseline.toml`.

Never grow a baseline to make a commit pass. See `AGENTS.md` Non-Negotiables.

## Layout

- `src/arch/` owns architectural checks.
- `src/style/` owns style checks.
- `src/idioms/` owns Rust-idiom checks.
- `src/common/` owns shared scope, violation, and baseline plumbing.
- `src/self_cache/` owns freshness, publication, locking, leases, and cleanup.
- `src/agent_hook/` owns hook payload routing and policy.

Each lint check is one file under `src/<namespace>/checks/` implementing the
`Check` trait.
