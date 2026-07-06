# Kithara Workflow

This file is the lazy-loaded workflow index for coding agents. `AGENTS.md` is the
always-on contract. Open this file when a task needs planning, split execution,
handoff, integration, or a workflow decision.

## Recommended Flow

- Start with quick discovery: confirm the goal, affected paths, and matching crate
  `README.md` / `CONTEXT.md` files.
- Use a `Task Packet` when coordination would otherwise be ambiguous. Small
  single-owner tasks can keep this lightweight.
- A non-trivial `Task Packet` is incomplete until `Constraints`, `Non-goals`, and
  `Validation scope` are named.
- Keep the primary acceptance target visible. After local fixes, check whether the
  top-level goal moved, not just a nearby symptom.
- When behavior changes, prefer a small TDD loop: failing contract test, minimal
  fix, then refactor.
- Split only when write scopes are independent. Freeze shared boundaries before
  parallel work starts.
- When work changes hands, use the handoff shape from `AGENTS.md` and state what
  was or was not validated.

## Load Only What Applies

- `docs/guides/rule-placement.md`: deciding where a new agent rule belongs.
- `docs/guides/red-flags.md`: expanding the `AGENTS.md` red-flag gate.
- `docs/guides/rust-shape.md`: Rust idioms, standard traits, loops, errors, and
  ownership shape.
- `docs/guides/architecture-shape.md`: owners, `Arc`, atomics, channels, call
  flow, and god objects.
- `docs/guides/review-validation.md`: tests, review discipline, PR readiness, and
  validation scope.
- `docs/guides/test-harness.md`: test entrypoints, scoped probes, regression
  proof, and shared harness helpers.
- `docs/guides/cancel-policy.md`: cancellation hierarchy details.
- `docs/guides/lint-policy.md`: lint-suppression policy and sanctioned exception
  details.
- `docs/guides/tooling.md`: formatter, lint, dependency-audit, and external-tool
  ownership.
- `docs/guides/agent-hooks.md`: tool adapter hooks and command guard behavior.

If a lint fails, open only the reported rule or the matching reference file. Do
not pre-load the whole `ast-grep` or `xtask` tree.

## Cross-Domain Guardrails

- Do not push surface-specific behavior into shared crates. Browser, Apple,
  Android, FFI, demo UI, and other surface concerns stay in adapter or app layers
  unless the behavior is truly shared.
- Keep protocol-specific logic in protocol crates. Shared crates own reusable
  stream, storage, decode, playback, and runtime abstractions, not HLS- or
  file-specific policy.
- Treat crate boundaries as architectural seams, not folders. If a change seems
  to belong in two layers, stop and name the owner.
- Tests and tooling validate contracts but should not silently redefine them.
  Fix the owned runtime or API boundary first, then update fixtures, harnesses,
  or workflow docs to match.
- When more than one coordinate or state space exists, add an explicit translation
  boundary. Do not move raw values freely between metadata, committed,
  virtual-reader, playback-time, or similar spaces.
- Prefer explicit state or session objects over free-floating `pending_*` or
  shadow flags when control flow spans multiple steps or owners.
- Before proposing a large rewrite, check whether a minimal fix or structural
  middle path can solve the owned bug without moving the contract.

## When A Full Plan Is Required

Use `docs/plans/_template.md` when the task:

- changes public API or shared types;
- changes workflow, tooling, hooks, or docs that affect multiple agents;
- affects wasm or perf-sensitive behavior;
- uses split execution.

Create or update the plan in `docs/plans/YYYY-MM-DD-<slug>.md`.

## Validation Ownership

Validation details are secondary in this document. Use repo commands and crate
READMEs as the source of truth for which commands to run.

What matters for coordination:

- the `Task Packet` names the expected validation scope;
- each handoff states what was or was not checked;
- the final report lists the commands actually run.

## Worktree Policy

Use raw `git worktree` when multiple agents need isolated write scopes.
Keep heavy local jobs serialized when they target the same build outputs.
