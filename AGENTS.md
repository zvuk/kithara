# Kithara Agent Index

This file is the canonical repo-level contract for Codex, Claude Code, Cursor,
and other coding agents. It carries only what must stop a wrong design before
code is written. Everything else is routed to an owner.

## Authority

- `AGENTS.md` owns repo-wide invariants, routing, and the red-flag gate.
- `docs/workflows/rust-ai.md` owns task setup, split, handoff, integration, and
  the task packet, handoff, and final report shapes.
- `docs/guides/*` holds lazy-loaded reference guidance. `docs/README.md` maps
  every guide to the trigger that should open it; open nothing else.
- `docs/rules/*` holds tool-neutral entry rules used through tool shims.
- On conflict: `AGENTS.md` wins over `docs/*`, which wins over crate
  `README.md` and wiki explanations, which win over tool-specific shims.
- `docs/guides/rule-placement.md` decides where a new rule belongs.
- Read a scoped `AGENTS.md` before changing its crate. Add one only for a
  necessary pre-code prohibition not already enforced or covered here.

## Sources Of Truth

Every fact below has one owner. Link to the owner; do not restate it.

| Fact | Owner |
| --- | --- |
| Capability status, roadmap, blockers | [GitHub Projects board](https://github.com/users/gerasim13/projects/3) |
| Project architecture | [Wiki architecture](https://github.com/zvuk/kithara/wiki/kithara) |
| Crate contracts, invariants, lifecycle | [Owning crate wiki page](https://github.com/zvuk/kithara/wiki/Crates) |
| Toolchain, image, and tool versions | `.config/ci-pins.toml` |
| Lint thresholds and baselines | `.config/arch/`, `.config/style/`, `.config/idioms/` |
| Command surface | root `justfile` and `.config/just/` |
| Released behavior | `CHANGELOG.md` |

## Core Principles

- Minimal magic and hidden dependencies. Predictability, testability, and
  reproducibility come first, and components stay loosely coupled.
- Code and checks define executable behavior. Wiki pages explain cross-file
  contracts and decision rationale; do not copy implementation inventories or
  task status there. Comments document their local item, and README files
  provide a brief overview, usage, and a link to the owning wiki page.
- Keep source code and doc comments self-contained. Do not link to wiki pages
  or repository documentation from source; route readers through README and
  agent instructions.

## Non-Negotiables

- No speculative code. Do not add helpers, branches, or abstractions that the
  current task does not use, and choose the simplest implementation that fully
  meets the requirements.
- Workspace-first dependencies. Add versions only in the root workspace and
  reuse existing crates when possible.
- Do not add `[patch.crates-io]` overrides to workspace manifests or Cargo
  configuration. Declare publish-compatible dependencies directly.
- Encoded/container media types live in `kithara-stream`. Do not duplicate
  `AudioCodec`, `ContainerFormat`, or `MediaInfo` elsewhere. Decoded-audio
  signal value types and pure sample/time math live in `kithara-signal`.
- Do not use `unwrap()` or `expect()` in production code without a strong,
  explicit reason.
- Name the canonical owner before changing shared state, shared types, or
  cross-crate contracts. If the owner is unclear, stop and clarify.
- Place a new shared type by the first rule that answers, in order: the
  standard library or an already-used dependency already provides it, so write
  no type; a crate owns the invariant the type expresses, so it goes there
  even when the shape looks general; the type is a self-contained mechanism
  with no domain, so it gets its own small crate named after that mechanism.
  A name like `common`, `core`, or `utils` means the owner was never found:
  go back to the second rule.
- Do not introduce parallel mutable sources of truth. When old and new state
  must coexist, stage the ownership transfer in the task packet or plan.
- Do not grow the architecture out of fallback paths. A fallback is a branch
  selected by the failure or absence of another path that produces the same
  result, so one job gets two execution paths; the parallel-chain section of
  `just lint similarity` lists candidates to check. When a case fails, extend
  the owner's algorithm to cover it instead of adding a path per failure; each
  such path is one more layer, and they stack. Syntax is not the test:
  `or_else` or an extra `match` arm is fine when a domain input selects it. A
  designed fallback (user-facing default, optional config, degraded mode) is
  part of the owner's contract and is justified in the owning crate wiki page
  or the task packet; a test that codifies any other fallback protects a
  symptom.
- Prefer generics and composition over near-duplicate protocol-specific types.
- Use `tracing`, not `println!` or `dbg!`, in production code.
- Do not use destructive git commands unless the user explicitly asks for them.
- Cancel-token hierarchy is typed and propagate-down. Hard-coded
  `CancelToken::root()` and `CancelToken::never()` are forbidden outside
  sanctioned owner sites. See `docs/guides/cancel-policy.md`.
- Zero tolerance for lint suppressions and baseline growth. An unavoidable lint
  means STOP and fix the underlying code. See `docs/guides/lint-policy.md`.
- Optimize for performance in hot paths. See `docs/guides/performance.md`.
- Use repo harnesses for acceptance and formatting: `just test` and `just fmt`.
  Raw `cargo test`, `cargo nextest`, or direct formatter commands are scoped
  probes only, not final validation claims.
- Production types and functions must not gain fields, methods, or behavior
  solely for tests. Attach USDT probes only to real operations; never add a
  no-op function just to expose a probe point.
- Do not preserve backward compatibility.

## Command Routing

Use these exact paths. Do not spend a turn on `just --list`; the root `justfile`
exposes domain modules only, and recipes live under `.config/just/`.

- Format: `just fmt`; check-only `just fmt check`.
- Compile and Clippy: `just check`; `just check clippy`.
- Lint: `just lint`; `just lint fast`; `just lint gate`; `just lint full`.
- Autofix: most ratchets rewrite under `--fix` (`arch` also needs `--apply`).
  Reach for it before hand-editing; `docs/guides/tooling.md` lists them.
- Duplication and parallel-chain report: `just lint similarity [<crate>/src ...]`.
- Test: `just test`; `just test run <args>`; `just test all` adds doc-tests.
- UI suites: `just test ui`; through a real window, `just test ui-window`.
- CI: `just ci gate`; `just ci audit <scope>`; `just ci health`;
  `just ci report --artifacts <dir>`.
- Repeated-run evidence: `just ci stress <args>`; `just ci stress-report <args>`.
- Architecture: `just arch viz` and its scope flags.
- Quality: `just quality lab list|run <profile>`; `just quality coverage-risk`;
  `just quality assess`.
- Platforms: `just platform apple|android|wasm ...`.
- Cached xtask access is exceptional: `just tooling xtask <subcommand>`.

Read the printed `manifest.json` before using `arch viz` or `quality assess` as
evidence; `docs/guides/tooling.md` owns their flags.

## Agent Red-Flag Gate

Reject a design before coding when it:

- Has no canonical owner for touched state, shared types, or cross-crate
  contracts, or creates multiple mutable sources of truth.
- Answers a failure with a new fallback, retry, sentinel, or workaround path
  instead of fixing its cause.
- Crosses platform, protocol, surface, test, or crate-layer boundaries without
  owning the contract.
- Widens public API or adds ad-hoc Rust shapes instead of standard traits,
  domain types, config, or builders.
- Uses `Arc<...>`, especially `Arc<Atomic*>`, as ownership glue instead of an
  owner, command, or snapshot model.
- Introduces shared mutable god-state, globals, god objects, callback spirals,
  or unrelated responsibilities in one file, type, trait, or facade.
- Requires a lint suppress, new baseline entry, or "temporary" bypass to pass.
- Requires every change to update a repository-wide document. Track active
  work in issues on the GitHub Projects board instead.

`docs/guides/red-flags.md` expands this gate for non-trivial work.

## Definition Of Done

A change is done only when all of these hold:

- A test that failed before the change now passes, and it pins the contract
  rather than an incidental detail.
- `just fmt check` and `just lint fast` are clean, with no new baseline
  entries and no lint suppressions. Both `lint fast` (the commit hook) and
  `lint full` run `style` through `_shared`. A review lints through
  `lint gate`, which adds this tool's own tests. The `linux-lint` gate runs
  `lint full` on main and branch pushes.
- The acceptance target named in the task packet passes, and the claim cites
  harness output, not a scoped probe.
- Update the owning wiki page only when its described contract changes;
  unrelated changes require no documentation edit. Track unfinished work in
  issues, not explanatory pages or repository progress documents.

## Working Rules

- One task in flight per session. Finish it or hand it off before opening a
  second front in the same files.
- Start from the task packet in `docs/workflows/rust-ai.md` when the task is
  non-trivial, shared, or coordinated; a small single-owner task goes direct.
- Treat a non-trivial task packet as incomplete until `Constraints`,
  `Non-goals`, and `Validation scope` are filled in.
- Read only the docs and crate files that match the owned paths.
- A task that needs a plan follows `docs/plans/_template.md`.
- If shared boundaries are unclear, stop and clarify before implementation.
- Do not restate a repo rule in tool-specific files; route to the canonical doc.

## Resolving Rule Conflicts

- If a product requirement conflicts with these rules, discuss the compromise
  first and update `AGENTS.md`.
- Any forced rule bypass is explained briefly in the owning crate wiki page.
