# Agent Hooks

Use this only when touching tool adapters, hook behavior, or command routing.
Hooks are workflow guards, not code-style policy. Architecture and Rust shape are
still enforced by `ast-grep` and `cargo xtask lint`.

## Install

Run `cargo xtask agent-hook install` once for each concrete Git worktree and
again whenever an installed hook reports that it is stale. The command copies
the current `xtask` executable and its source fingerprint into a complete
versioned generation under `<worktree-git-dir>/kithara-agent-hook/`, then
atomically switches the `current` symlink. Installation is explicit: tool
adapters and the checked-in launcher never invoke Cargo or start a build.

An absent cache prints the install command and skips the guard. A stale cache
prints the same instruction and continues with the last-good policy binary.
These hooks protect workflow conventions; they are not a security boundary.

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
- other `.toml` -> `taplo format`
- `.json` / `.jsonc` -> `tidy-json --write`

`Cargo.toml` is deliberately skipped because its canonical workspace-wide
dependency-order rewrite requires `cargo xtask format --only manifest
--allow-dirty`. Run that command explicitly. The hook does not run Cargo, tests,
lints, markdown formatting, or architecture checks.

## Runner Cache

The checked-in runner resolves the current checkout and its concrete worktree
Git directory, then directly executes the installed binary. It contains no
fingerprinting, locking, Cargo invocation, build target, or process-management
logic. The runner exports the resolved checkout and cache paths so Rust owns
path containment and freshness validation.

The runner only accepts the versioned `current` generation, so flat caches from
older launchers are treated as absent. Failed or concurrent installs cannot
expose a binary from one generation with another generation's fingerprint. On
later installs, inactive generations older than one hour are pruned; the active
and recent generations remain available across an in-flight launcher handoff.
Orphaned temporary generations and pointers from an interrupted install use the
same age-safe cleanup.

The fingerprint covers `xtask/src/agent_hook.rs`, the `agent_hook` module tree,
`xtask/src/main.rs`, the xtask manifest, optional Cargo/toolchain configuration,
and the host OS and architecture. Unrelated xtask command sources and the root
workspace manifest or lockfile do not invalidate the policy binary.

`cargo xtask agent-hook pre-bash|post-edit` remains available as an explicit
diagnostic entry point. Tool adapters must use `xtask/agent-hook` so installed
hook invocations remain Cargo-free.

## Tool Adapter Rule

Tool-specific files such as `.claude/settings.json` should only call repo-owned
commands or route to canonical docs. Do not duplicate policy text there.
