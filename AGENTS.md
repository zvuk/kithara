# Kithara Agent Index

This file is the canonical repo-level contract for Codex, Claude Code, Cursor, and other coding agents.
Use it for repo-wide coding conventions, path routing, and stable coordination shapes.

## Authority

- `AGENTS.md` is the canonical repo-level contract for all coding agents. It owns repo-wide invariants, coding conventions, routing, and the stable coordination shapes below.
- `.docs/workflow/rust-ai.md` is the canonical workflow for task setup, split, handoff, and integration.
- If two documents disagree: `AGENTS.md` wins over `.docs/workflow/*`, which wins over crate `README.md`, which wins over tool-specific shims.

## Core Principles

- Minimal magic and hidden dependencies.
- Predictability, testability, and reproducibility.
- Components should stay loosely coupled and replaceable.
- Code is the source of truth. Longer contracts and invariants belong in the owning crate `README.md`.

## Non-Negotiables

- No speculative code. Do not add helpers, branches, or abstractions that are not used by the current task.
- Workspace-first dependencies. Add versions only in the root workspace and reuse existing crates when possible.
- Shared media types live in `kithara-stream`. Do not duplicate `AudioCodec`, `ContainerFormat`, or `MediaInfo` elsewhere.
- Do not use `unwrap()` or `expect()` in production code without a strong, explicit reason.
- Name the canonical owner before changing shared state, shared types, or cross-crate contracts. If the owner is unclear, stop and clarify before implementation.
- Do not introduce parallel mutable sources of truth without an explicit transition contract. When old and new state must coexist temporarily, use a staged ownership transfer in the task packet or plan.
- No fallback chains (`try A, else B, else C`) to paper over state-resolution bugs. A fallback is a code smell: if the primary path has no correct answer, the underlying state contract is broken — fix the contract, not the symptom. Legitimate fallbacks (user-facing defaults, optional config, degraded-mode operation) must be explicitly justified in the owning crate `README.md` or the task packet. Tests that codify fallback behaviour protect symptoms and should be treated as evidence of a root-cause bug.
- Prefer generics and composition over near-duplicate protocol-specific types.
- Use `tracing`, not `println!` or `dbg!`, in production code.
- Do not use destructive git commands unless the user explicitly asks for them.
- Prefer clean, maintainable code over clever shortcuts or speculative abstractions.
- Keep code readable and easy to understand.
- Optimize for performance in hot paths.


## Dependency Management

### Workspace-first

- Declare dependency versions only in the root `Cargo.toml` under `[workspace.dependencies]`.
- Reference dependencies from crates with `{ workspace = true }`.
- Do not add duplicate or overlapping crates when the workspace or standard library already covers the need.

### Dependency hygiene

- Do not add temporary dependencies without a real need.
- Do not pull in a heavy crate for a small utility without checking cost first.
- If a new dependency is unavoidable, justify it in the task, plan, or PR description and add it to `[workspace.dependencies]` first.

## Coding Conventions

### Imports and qualified paths
- Keep production code in `src/` and substantial fixtures in `tests/`.
- Keep `use` imports at the top of the file. Do not place `use` inside functions, methods, or blocks.
- Prefer short readable names in the body over repeated deep qualified paths.
- Full paths are acceptable only when they clearly improve readability, such as resolving a name conflict.
- Default visibility is `pub(crate)`. Public named-field types exposed across crates should be `#[non_exhaustive]`.

### Naming

- Choose the simplest and shortest names that still describe the real meaning.
- Prefer standard, obvious words such as `open`, `new`, `get`, `put`, `read`, `write`, `seek`, `stream`, `send`, and `recv` when they fit.
- Avoid clever or overly long names that encode implementation history instead of meaning.

### Comments and documentation
- Keep in-code comments short and local.
- Do not add large comment blocks at the top of files.
- Do not restate the obvious in comments.
- Contracts, invariants, lifecycle, protocol, or cache explanations belong in the owning crate `README.md`.
- NO separator comments or banner commends and no ad-hoc style variants that conflict with workspace lint rules.
- Keep comments short and local. Put longer contracts and invariants in crate `README.md`.

### File size and decomposition

- Do not let a single `.rs` file grow into a dump of abstractions.
- Extract large types, big `impl` blocks, or distinct subsystems into their own files or modules.
- Prefer `mod.rs` plus focused sibling files over one oversized source file.
- `lib.rs` and `mod.rs` should contain only module declarations and re-exports.

### Tests and fixtures

- `src/` is for production code, not for a growing fixture warehouse.
- Large fixtures, local servers, generated content, and multi-step scenarios belong in `tests/`.
- Small unit tests and tiny helpers under `#[cfg(test)]` are fine next to the code.

## Test-Driven Development

- Behavior changes should be driven by tests that describe the intended contract.
- Tests must be deterministic.
- Tests must not depend on the external network.
- Tests should capture the contract, not an incidental implementation detail.
- Test logs and generated data should stay at a reasonable size.

## Generic Programming

- Prefer standard and `tokio` abstractions when they fit instead of inventing near-identical custom traits.
- Extend behavior through type parameters, traits, and composition instead of copy-paste specialization.
- Do not create near-duplicate HLS and file types when the difference can be expressed generically.
- Avoid large "god traits". Prefer several smaller traits with clearer ownership.

## Visibility And API Surface

### Default visibility

- New items should be `pub(crate)` unless they are part of a documented public contract.
- Do not use bare `pub` for internal helpers or implementation details.
- When promoting something to `pub`, verify that it is intentional, documented, and covered by tests.

### Public types

- Public enums and public named-field structs exposed across crate boundaries should be `#[non_exhaustive]`.
- Small, obviously stable exceptions are allowed when extension is unlikely and direct construction is part of the intended contract.

### Shared types

- Shared types such as `AudioCodec`, `ContainerFormat`, and `MediaInfo` live in `kithara-stream`.
- Before introducing a new shared type, search the workspace and reuse an existing canonical type when possible.

## Loose Coupling And Modularity

- Keep cache, network, orchestration, decode, and playback responsibilities separate.
- Avoid circular dependencies.
- Dependencies should point downward toward base abstractions, not back upward into higher-level orchestration.
- Facade layers may aggregate components, but should not hide "smart" logic that belongs lower in the stack.
- External code must not depend on internal cache layout or a specific HTTP client unless that is explicitly part of the contract.

## Linting, Formatting, And Unified Settings

- Respect workspace-wide config such as `rustfmt.toml`, `clippy.toml`, `deny.toml`, and `sgconfig.yml`.
- Change lint policy in the shared config files instead of creating per-crate drift unless there is a strong reason.
- The pre-commit hook expects clean formatting, linting, and tests. If they fail after your change, assume your change caused it until proven otherwise.
- Treat **`rustc` warnings** (for example `unused-imports`, `dead_code`, `deprecated`, unfulfilled `#[expect]`, `unreachable_pub`) the same as Clippy: fix the cause in code you touch—remove dead code, update imports and APIs, replace deprecated items—rather than leaving warnings to accumulate.
- Prefer fixing the cause of a compiler or Clippy warning (correct types, control flow, dependency or API change) over silencing it. Do not add `#[allow(...)]`, file-level `#![allow(...)]`, or other lint suppressions unless there is no reasonable code-side fix; if you must suppress, document why in the owning crate `README.md` or in a short comment on the same item.

### Style rules enforced by tooling

- No separator comments such as `// ====`.
- Import names at the top of the file rather than using inline qualified paths in function bodies.
- `lib.rs` and `mod.rs` should contain only module declarations and re-exports.

## General Quality Rules

- Errors should be typed and include context about what failed and on which resource.
- Logs should be useful and should not leak secrets.
- Use `tracing` fields for context such as `asset_id`, `url`, `resource`, `variant`, `segment_idx`, `bytes`, `attempt`, or `timeout_ms`.
- Do not leave temporary prints in production code.
- Any public API change should come with tests that capture the contract.

## Coordination Shapes

<task_packet>
Goal:
Affected paths:
Read first:
Same-as example:
Constraints:
Non-goals:
Expected output:
Validation scope:
Split proposal:
</task_packet>

<split_policy>
Prefer split execution when write boundaries are explicit and independent.

Every split task must define:
- owned paths per agent
- forbidden paths per agent
- required reads per agent
- sequencing dependencies
- one integrator owner

Do not split when two agents would contend on the same file, the same shared type, or the same unresolved design boundary.
</split_policy>

<handoff_contract>
Done:
Remaining:
Touched paths:
Decisions made:
Validation:
Open risks:
</handoff_contract>

<final_report>
Changed files:
Commands run:
Risks or follow-ups:
</final_report>

## Working Rules

- Start from a task packet when the task is non-trivial, shared, or likely to benefit from explicit coordination.
- Small single-owner tasks with a clear boundary can proceed directly without a task packet.
- Treat a non-trivial task packet as incomplete until `Constraints`, `Non-goals`, and `Validation scope` are filled in.
- Keep the primary acceptance target explicit in the packet or plan and revisit it after local fixes.
- Read only the repo docs and crate READMEs that match the owned paths.
- The stable task packet, handoff, and final report shapes live here. Do not create parallel template docs for them.
- If a task needs a plan, follow `.docs/workflow/rust-ai.md` and `.docs/plans/_template.md`.
- If shared boundaries are unclear, stop and clarify before implementation.
- Keep debate procedures, investigation journaling, and TDD choreography out of `AGENTS.md`. Put that guidance in workflow docs or owning `README.md` files.
- Do not restate the same repo rule in tool-specific files. Tool-specific files should only route the agent to canonical docs and scoped domain guidance.

## Resolving Rule Conflicts

- If a product requirement conflicts with these rules, discuss the compromise first and update `AGENTS.md`.
- Any forced rule bypass should be explained briefly in the owning crate `README.md`.
