---
name: quality-assessment
description: "Build and interpret evidence-backed repository quality assessments for a Rust workspace, crate, or canonical module. Use when asked for code health, architecture complexity, refactor signals, technical-debt status, a full or deep analysis, comparison with an earlier report, or a decision-oriented report that combines architecture, similarity, lint baselines, CRAP, Quality Lab, tests, dependencies, concurrency, performance, and platform evidence."
---

# Quality Assessment

Build the deterministic repository assessment first, then add source-aware
interpretation. Keep the tool verdict and agent interpretation separate.

## Select the command

Use the exact route below. Never spend time discovering it with `just --list`.

```text
just quality assess [OPTIONS]
```

Map the request to options:

| Request | Options |
| --- | --- |
| Production workspace, normal health report | no options |
| Every workspace surface, including integration tests and tooling | `--profile complete` |
| Rare heavyweight analyzers, coverage/CRAP, perf, concurrency, and platforms | `--depth deep` |
| One crate | `--crate <cargo-package>` |
| One module subtree | `--module <cargo-package>::<module-path>` |
| Compare with an earlier assessment | `--baseline <assessment.json-or-directory>` |
| Re-render only from existing compatible artifacts | `--reuse-existing` |

Interpret "full", "deep", or "all tools" as `--depth deep`. Add
`--profile complete` only when the user asks for the entire workspace,
integration tests, test utilities, xtask/devtools, or every source surface.
An explicitly selected crate or module is included even if project defaults
normally exclude it.

Do not use `--reuse-existing` for a requested fresh or deep analysis. It is for
an explicit artifact-only request or a deliberate re-render.

Project policy marks workspace-wide `cargo-mutants` as `not-applicable`.
Never run it automatically. If mutation testing is explicitly requested,
require a bounded crate or module scope first.

## Build and verify the assessment

1. Confirm the exact checkout and worktree before running anything.
2. Run the mapped `just quality assess` command and retain the printed report
   path.
   - Standard runs portable gates as separate attributable stages.
   - Deep runs the full health pipeline and every applicable configured
     heavyweight stage.
3. Read `manifest.json` in the same directory first.
   - `complete`: continue.
   - `partial`: report the broken stages from `stages/*.json` and their logs;
     do not make a complete health claim.
4. Read `assessment.json` for exact values and `assessment.md` for the
   decision-oriented view.
5. Verify the tool coverage matrix. Every relevant tool must be classified as
   `executed`, `reused`, `covered-by`, `not-applicable`, or `evidence-gap`.
   Never silently omit a gap.
6. Follow evidence links to the canonical owner instead of recomputing its
   metric:
   - architecture: `target/architecture/<revision>/`;
   - similarity: `target/similarity/<revision>/`;
   - health and test structure: `target/health-report.md` and
     `target/quality-report.md`;
   - heavyweight analyzers and CRAP:
     `target/quality-lab/<revision>/`;
   - stage commands and logs: the assessment's `stages/` and `logs/`.

The assessment revision includes a content/diff digest for a dirty worktree.
Do not claim that a committed Cha artifact covers dirty content. Trust the
assessment coverage status. Do not treat an unversioned global report as
current merely because it exists; fresh declared artifacts must be created or
updated by their stage.

## Interpret the deterministic verdict

Existing arch/style/idioms baseline entries are technical debt, not accepted
quality. The target is zero.

- Workspace refactor threshold: 100 debt units.
- Crate/module threshold:
  `max(1, ceil(100 * scope_LOC / workspace_LOC))`.
- Any baseline growth is a debt regression.
- A hard invariant, a debt threshold/regression, or corroboration at the same
  source location by two independent tools can produce `refactor`.
- A single diagnostic tool produces `investigate`, not proof that a refactor is
  correct.
- `evidence-gap` means evidence must be restored before a health claim.
- `stable-with-debt` means the scope is below its proportional threshold, not
  that the debt may grow.
- `healthy` requires zero debt and complete applicable evidence.

ACI and its component metrics are diagnostic. Do not invent an ACI threshold or
turn one high score into a gate. Use ACI to rank contours, then corroborate with
cycles, ownership, propagation, similarity, lint debt, CRAP, test failures, or
another independent signal.

Similarity scores are candidates, not behavioral equivalence. Check the
reported type substitutions, caveats, field matches, shared behavior, and real
source before proposing merge, composition, or generic extraction.

## Deep contextual analysis

When the user requests deep analysis, do more than repeat the generated
Markdown:

1. Start from the highest-debt files, highest-ACI contours, hard invariants,
   baseline regressions, CRAP risks, and independently corroborated locations.
2. Inspect the top ambiguous source locations and their canonical owner,
   callers, owned resources, data flow, and public boundary.
3. Prefer a bounded refactor boundary where multiple signals overlap.
4. Distinguish a confirmed issue from an inference, and name the evidence for
   each conclusion.
5. Use the complete tool coverage matrix, including baseline findings. Missing
   or skipped tools are part of the result.

Do not update lint baselines, edit production code, start a refactor, commit, or
push unless the user explicitly requests that next action.

## Report back

Lead with:

1. deterministic tool verdict and assessment status;
2. scope/profile/depth and revision;
3. debt versus threshold and baseline delta;
4. hard invariants and corroborated hotspots;
5. highest architecture contours and the metrics that make them worth
   inspecting;
6. the smallest useful refactor boundaries;
7. evidence gaps or broken stages;
8. a separate section labelled as agent interpretation.

Link the assessment report, its JSON, the relevant Mermaid architecture page,
and the canonical source artifacts. Preserve the distinction between an
automatic signal and a source-level judgment.
