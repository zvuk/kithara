# Agent Hooks

Use this only when touching tool adapters, hook behavior, or command routing.
Hooks are workflow guards, not code-style policy. Architecture and Rust shape are
still enforced by `ast-grep` and `cargo xtask lint`.

## Pre Bash Guard

`xtask/agent-hook pre-bash` reads agent hook JSON from stdin and denies
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

`xtask/agent-hook post-edit` formats only the reported file for known edited
file classes:

- `.rs` -> nightly `rustfmt` with child-module traversal disabled
- `Cargo.toml` -> `cargo xtask format --only manifest --allow-dirty`
- other `.toml` -> `taplo format`
- `.json` / `.jsonc` -> `tidy-json --write`

The manifest path is the deliberate exception to the Cargo-free edit path: it
keeps the canonical workspace-wide dependency-order rewrite instead of copying
that policy into the hook tool. The hook does not run tests, lints, markdown
formatting, or architecture checks.

## Runner Cache

The checked-in runner builds the full `xtask` package once into a cache owned by
the current Git worktree. Its fingerprint covers the xtask source, package
manifest, Cargo config, toolchain file, and host platform. The steady-state path
executes the cached binary directly and dispatches the hook before loading the
normal xtask context. Bootstrap Cargo runs only when that cache is missing or
stale; `post-edit` for `Cargo.toml` remains the separate formatting exception
described above.

Workspace manifest and lockfile changes intentionally do not invalidate the
already linked policy binary. When hook policy inputs do change, bootstrap uses
`cargo build --locked`, so it never rewrites the workspace lockfile.

The bootstrap uses the host's kernel file lock, defaults Cargo to four build
jobs unless `CARGO_BUILD_JOBS` is already set, and removes its temporary target
after copying the binary. The build runs in its own process group so an
interrupted bootstrap terminates Cargo and its descendants before cleanup. A
cached post-edit binary is preserved long enough to format a just-changed
manifest. Tool adapters locate the runner by walking up from the host project
directory using shell builtins, so neither a nested session directory nor Git
discovery adds work to the hot path.
The runner passes its resolved checkout root to the binary as the canonical
path-containment owner; only the compatibility `xtask` entry point falls back to
the hook payload directory.

`cargo xtask agent-hook pre-bash|post-edit` remains a direct entry point, but tool
adapters must use the checked-in runner so routine commands do not enter Cargo's
global package-cache lock.

## Tool Adapter Rule

Tool-specific files such as `.claude/settings.json` should only call repo-owned
commands or route to canonical docs. Do not duplicate policy text there.
