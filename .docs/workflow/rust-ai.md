# Kithara Workflow

This repository uses a local-first, split-friendly workflow for coding agents.
`AGENTS.md` holds repo-wide invariants and stable coordination shapes; this file describes the default execution flow.

## Recommended artifacts

Use the reasonable set of artifacts that keeps the task understandable and easy to integrate:

1. `Task Packet`
2. `Split Map`
3. `Agent Assignment`
4. `Handoff`
5. `Integration Summary`

- Small single-owner tasks can use only a `Task Packet`.
- `Split Map` and `Agent Assignment` matter mainly for multi-agent work.
- `Handoff` matters only when work moves between agents.
- `Integration Summary` matters most for split work or broader diffs.
- The field shapes for `Task Packet`, `Handoff`, and `Final Report` live in `AGENTS.md`; do not fork them into tool-specific or parallel template docs.

Prefer these artifacts over long free-form summaries when coordination matters.

## Recommended flow

This is a default flow, not a rigid protocol. Collapse it when the task is small and the boundary is already clear.

- Start with a quick discovery pass: confirm the goal, identify affected paths, and load the matching crate `README.md` files plus any repo-level docs that `AGENTS.md` routes you to.
- Use a `Task Packet` when coordination would otherwise become ambiguous. Small single-owner tasks can keep this lightweight.
- Treat a non-trivial `Task Packet` as incomplete until `Constraints`, `Non-goals`, and `Validation scope` are named.
- Keep a primary acceptance target visible. After local fixes, check whether they moved the top-level goal rather than only a nearby symptom.
- When behavior changes, prefer a small TDD loop: define the desired behavior with a test, confirm the test fails for the expected reason, implement the smallest fix, then refactor after tests pass.
- Split only when write scopes are clearly independent. Freeze shared boundaries before parallel work starts.
- When work changes hands, use the handoff shape from `AGENTS.md` and state what was or was not validated.
- For split work, keep one integrator owner who verifies the final diff still matches the packet and produces the final report.

## Cross-domain guardrails

These are recurring project-specific boundaries that matter more than generic process detail:

- Do not push surface-specific behavior into shared crates. Browser, Apple, Android, FFI, demo UI, and other surface concerns should stay in the corresponding adapter or app layers unless the behavior is truly shared.
- Keep protocol-specific logic in protocol crates. Shared crates should own reusable stream, storage, decode, playback, and runtime abstractions, not HLS- or file-specific policy.
- Treat crate boundaries as architectural seams, not just folders. If a change seems to belong in two layers, stop and check which crate actually owns the contract.
- Tests and tooling validate contracts but should not silently redefine them. Fix the owned runtime or API boundary first, then update fixtures, harnesses, or workflow docs to match.
- When more than one coordinate space or state space exists, add an explicit translation boundary. Do not move raw values freely between metadata, committed, virtual-reader, playback-time, or similar spaces.
- Prefer explicit state or session objects over free-floating `pending_*` or shadow flags when control flow spans multiple steps or owners.
- Before proposing a large rewrite, check whether a minimal fix or structural middle path can solve the owned bug without moving the contract.

For detailed crate-level rules, follow `AGENTS.md` routing and read the owning `README.md` files.

## When a full plan is required

Use `.docs/plans/_template.md` when the task:

- changes public API or shared types;
- changes workflow, tooling, hooks, or docs that affect multiple agents;
- affects wasm or perf-sensitive behavior;
- uses split execution.

Create or update the plan in `.docs/plans/YYYY-MM-DD-<slug>.md`.

A small single-owner change with a clear boundary can work from the `Task Packet` alone.

## Validation ownership

Validation details are secondary in this document. Use repo commands and crate READMEs as the source of truth for which commands to run.

What matters for agent coordination is that:

- the `Task Packet` names the expected validation scope;
- each handoff states what was or was not checked;
- the `Integration Summary` lists the commands actually run.

## Worktree policy

Use raw `git worktree` when multiple agents need isolated write scopes.
Keep heavy local jobs serialized when they target the same build outputs.
