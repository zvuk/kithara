# kithara-devtools — contracts

Owning-crate contracts for the reusable xtask command core. The [`README.md`](README.md)
is the overview; this file owns the invariants a consumer or contributor relies on.

## Ctx lifecycle

`Ctx::load()` resolves the workspace root once via `cargo metadata` and parses
`.config/xtask.toml` into `Ctx::config`. Every command takes `&Ctx`; a command
must not re-resolve the root or re-read the config file. `Ctx::config` is the
single parsed configuration for the run.

## Configuration contract

- The config file is `<workspace-root>/.config/xtask.toml` and is **optional**.
  A missing file yields `ProjectConfig::default()` — documented code defaults,
  not a fallback chain. This is the zero-config adoption path.
- `project.name` defaults from cargo metadata when the config omits it: the
  workspace-root package name, else the sole workspace package, else the
  workspace directory name. It is a human-facing label (report titles, temp-dir
  prefixes) — a sanctioned user-facing default, not state resolution.
- Every config struct carries `#[serde(default, deny_unknown_fields)]`. An
  unknown key in a core section is a typed parse error naming the offending
  token — misconfiguration fails loud, it is never silently ignored.
- **`[ext]` ownership rule.** The core schema names only generic concerns
  (`project`, `workspace_scan`, `lint_exclude`, `health`, `test`, `perf`,
  `orphans`, `quality`). Anything project-specific lives under `[ext.*]`, exposed as the
  raw `ext: toml::Table` passthrough. The core never interprets `[ext]`; the
  consuming bin deserializes its own typed view from it (kithara does this in
  `xtask/src/config.rs`). This keeps `deny_unknown_fields` strict on core
  sections while letting any project add its own without touching the core.
- `[workspace-scan] exclude` holds globs applied in `workspace_rs_files_scoped`
  (the lint scan path) so a project can drop media/venv/generated trees from
  every scanning namespace. Raw `walk_rs_files` stays a pure directory walk.
- `[perf]` owns the generic test-suite performance pipeline configuration:
  lane matrix entries (`flash`, `backend`), optional `primary_lane` for ranking
  and profile/report defaults, optional `frame_prefix` for gecko own-frame
  attribution, and `nextest_profile` (default `perf`). The selected nextest
  profile must expose `[profile.<name>.junit] path = "junit.xml"` because
  `perf matrix` copies `target/nextest/<name>/junit.xml` into the run data.
- `[quality.assessment]` owns only project-specific deep-stage execution:
  structured argv, owned tool names, expected artifacts, optional platform
  selection, and whether a stage belongs only to the complete profile. It does
  not declare source relationships or metric values. Delegated commands retain
  ownership of their existing budgets and output schemas.

## CLI surface

`CoreCommand` is a `clap::Subcommand` a consumer flattens into its own enum via
`#[command(flatten)]`; `run(&CoreCommand, &Ctx)` dispatches it. Command names,
flags, and help text are the public surface — treat changes to them as
API changes covered by the consuming project's expectations.

`viz` has no required nested subcommand. Its no-argument path builds one
lossless source-evidence graph, applies the selected scope and LOD, and writes
Mermaid, Markdown, contour JSON, metrics JSON, graph JSON, projection JSON, and a manifest below
`target/architecture/<revision>/`. `--view hierarchy` and `--view ownership`
are projections of that same graph, not independent analyzers. Static call
targets remain candidates until rust-analyzer resolves them through stable LSP
call hierarchy requests.

LOD is independent from scope. `auto` means LOD 0 for a workspace, LOD 1 for a
crate, and LOD 2 for a module. Explicit levels are crates (0), top-level
subsystems (1), modules and abstractions (2),
constructors/boundaries/resources (3), and the complete evidence graph (4).
Concrete types own their `impl` methods, traits are contracts connected by
`implements`, and free functions belong to one module-functions abstraction.
Workspace output automatically links per-crate diagrams and subsystem hotspots
selected by contracted boundary degree; crate output links every subsystem's
abstraction diagram. This page selection is navigation, not evidence
truncation: every source contour remains in `contours.json`.
Explicit workspace LOD 1 uses the same index-plus-pages shape rather than
placing every workspace module in one Mermaid block.

`--crate <package>` also projects the same full workspace graph. It isolates
the selected package, removes inter-package Cargo dependencies and incoming
callers, and retains concrete outgoing cross-package interactions as compact
ports labelled with the public target symbol. External packages are not
expanded. The projection requires neither a configured runtime scenario nor an
extra discovery command.

When a relation endpoint is hidden by LOD, the endpoint lifts to its nearest
visible owner. Equal visible endpoint/kind pairs aggregate while retaining the
original method pairs, occurrence count, evidence origins, and style in
`projection.json`. Relation kinds remain distinct. There is no diagram node
budget: LOD 4 is partitioned by semantic contours into an index and linked
pages, and manifest schema v4 records complete visible-node coverage plus
hierarchical artifact paths.
Optional and target-gated Cargo dependencies carry conditional evidence;
unconditional normal dependencies remain resolved structural facts.

The metrics profile is computed from the same contracted relations as Mermaid.
It reports coupling, cohesion, propagation, cycles, depth, bottlenecks,
ownership, boundary alignment, abstractness, main-sequence distance, and an
experimental ACI. Resolved static evidence owns the stable profile; candidates
and runtime observations remain separate comparisons. Boundary concentration
and external coupling are weighted by actual boundary load before they enter
the ACI. The ACI is diagnostic and does not alter existing CI budgets.

Project defaults under `[architecture.filters]` and repeatable
`--exclude-crate` / `--exclude-module` arguments compile into one additive
projection filter. It removes matching symbols before semantic selection and
removes matching contours plus incident edges from the `DiagramModel`; it does
not alter raw `graph.json` evidence or disable an excluded package used as a
runtime scenario. Manifest schema v4 records the effective patterns and
excluded counts. Relations never lift through an excluded endpoint.

`[architecture.runtime.scenarios]` is the only project-specific runtime
evidence input.
Its strict tagged schema accepts Cargo integration tests, Cargo binaries, and
existing trace paths. Test and binary targets are validated against Cargo
metadata and launched with structured arguments, a bounded timeout, captured
logs, and `ARCHITECTURE_TRACE_PATH`; no shell command is stored in config. With
no selector, `viz` runs every configured scenario. `--scenario <name>` runs one
and projects only nodes carrying that scenario's trace evidence.

Runtime producers use the public, domain-neutral `viz::trace` JSONL API.
Records carry versioned source, span, task, thread, correlation, and resource
identity. Cross-thread sends are connected only by an explicit correlation
identifier. Source matching enriches existing syntax nodes; unmatched records
remain visible runtime events rather than guessed static targets. Manual
`--trace` input uses distinct evidence styling.

Runtime enrichment precedes semantic resolution so a selected scenario limits
rust-analyzer work to source functions observed in that trace. Both enrich the
same graph before any view or prose is produced. The Markdown report is derived
from the visible `DiagramModel` and its contracted metrics graph; every finding
and relation points to a visible Mermaid contour.

Artifact status is `complete`, `truncated`, `static-only`, `runtime-enriched`,
or `incomplete`. Timeout, failed execution, malformed trace, or failed semantic
resolution preserves partial artifacts and makes the command fail. Missing
optional rust-analyzer yields static/runtime output unless
`--semantic required` was requested. Truncation is explicit, applies only to
evidence collection, and never removes nodes because of diagram size.

## Quality assessment contract

`quality assess` is an artifact federation layer. Existing linters,
architecture, similarity, health, Quality Lab, test, dependency, concurrency,
performance, and platform commands remain canonical owners; the assessment
normalizes and correlates their output without reimplementing their metrics.

The default profile is `product`; `complete` disables project-default
architecture and similarity exclusions and includes integration tests,
test/tooling crates, and other workspace surfaces. An explicitly selected
crate or canonical `package::module` scope is included even when defaults
exclude it. The default depth is `standard`; `deep` adds configured
heavyweight stages. Standard executes each portable gate separately so its
stage evidence is attributable and does not pay for the heavyweight sections
of `health`; deep runs the full health pipeline plus the registered rare
stages. Configured stages are advisory unless `hard_invariant = true` records
an already-established project gate.

Artifacts are deterministic JSON/Markdown plus a manifest under
`target/quality-assessment/<revision>/<profile>-<depth>/`, with stage JSON and
logs in sibling directories. A dirty worktree revision carries a content/diff
digest. Committed Quality Lab output, especially Cha, must not claim coverage
of dirty content.

The workspace debt target is zero and the refactor threshold is 100. A smaller
scope uses `max(1, ceil(100 * scope_LOC / workspace_LOC))`. Existing lint
baseline entries count as debt; baseline growth is a regression. A hard
invariant, debt threshold/regression, or same-location corroboration by two
independent tools can produce `refactor`. One diagnostic tool produces
`investigate`. ACI remains diagnostic and has no invented gate.

Verdicts are advisory and do not make a complete command fail. Invalid input
or broken required analysis preserves a partial artifact and returns an error.
The tool coverage matrix must account for every known signal as `executed`,
`reused`, `covered-by`, `not-applicable`, or `evidence-gap`.
Project policy may classify an impractical tool under
`[[quality.assessment.not_applicable_tools]]`; such a tool remains visible in
the matrix with its reason and is never scheduled as a deep stage.

## Behavioral similarity ownership

`similarity` owns native source-level comparison of Rust abstractions. The
existing `just lint similarity` entrypoint first parses the selected production
sources, writes `report.md`, `report.json`, `graph.json`, and `manifest.json`
below `target/similarity/<revision>/`, then runs the existing `similarity-rs`
function-copy profile. Native findings are diagnostic and do not change the
audit, advisory, or strict budgets owned by the external profile.

Type shapes are interned bottom-up and pair comparisons are memoized for the
run. Generic parameter names normalize by position, derive attributes do not
change source shape, nested container arguments compare recursively, and
`SmallVec<[T; N]>` / `ArrayVec<[T; N]>` expose `T` as the container element.
Built-in `std` families carry conservative similarity degrees. Dependency
families activate only when Cargo metadata shows the dependency, and
`.config/similarity.toml` may add project-specific families or directional
pair relations with substitution caveats.

Behavior is a normalized source graph over signatures, control flow, calls,
field access, effects, and literal values. Generic and local names are erased
where they do not carry semantics; domain types, constructors, significant
macro symbols, and effects remain. Candidate buckets precede three rounds of
Weisfeiler-Lehman refinement and bounded method alignment. `impl` blocks in
separate files attach only when their owner resolves uniquely in the workspace
or crate. Partial state overlap without matching behavior is a review finding;
composition is recommended only when aligned `impl` behavior supports it.

`report.json` and `graph.json` are exhaustive. Manifest schema v2 records the
exact roots and whether project-default exclusions were disabled, so an
assessment never reuses evidence from another profile or scope. The Mermaid
view aggregates candidates by crate pair, so rendering remains useful without
imposing a node or finding limit. Strict includes test paths and `#[cfg(test)]`
items; audit and advisory keep the established production-only policy.
Proc-macro output is not expanded, similarity never proves substitutability,
and caveats must be checked before refactoring.

## Quality Lab ownership

`quality lab` owns heavyweight external code analysis that must stay outside
`lint-fast`, the normal audit, and pre-commit. Its required
`.config/quality-lab.toml` is loaded exactly once by the command and has a
strict schema independent of the general `.config/xtask.toml`.

- `coverage` owns the `cargo-crap` coverage-risk gate. A production run without
  `--baseline` emits the absolute JSON artifact; a pull-request run supplies
  that artifact and gates regressed entries plus new functions above CRAP 30.
  The wrapper checks delta JSON because cargo-crap's `--fail-regression` exit
  code does not include new functions. A failing instrumented test run still
  writes Cobertura and LCOV and runs cargo-crap; the combined stage preserves
  the test exit as findings instead of losing the risk evidence.
- `scheduled` runs Cha history/layers/smells, rustqual test-quality checks, and
  cargo-dupes sub-function duplication. Findings are advisory; missing tools,
  invalid reports, version mismatches, and timeouts are tool errors.
- `manual` adds read-only PMAT `repo-score --format json --deep`. Missing
  executables are `skipped`; findings stay advisory. A direct non-coverage tool
  run follows this manual policy.
- Every external version must match the exact pin before analysis. Native
  output, stderr, per-tool `manifest.json`, and JSON/Markdown summaries live
  below `target/quality-lab/<revision>/`.
- Cha runs only from a clean, non-shallow source worktree and analyzes a
  disposable local clone. The clone is deleted after the run so Cha cache state
  cannot leak into the source checkout.

KISS is not executed directly because it overlaps the existing stack and
writes hidden user-level state. PMAT remains manual because its breadth and
runtime make it unsuitable for a routine gate. Promoting a unique external
check into `syn`, Cargo metadata, Git, or ast-grep requires repeated actionable,
deterministic evidence and two comparison runs before retiring the adapter.

## Feature gating

`lint` (arch/style/idioms/lint) and `viz` are default-on cargo features that
gate the syn-heavy command modules. Gates live only at the `lib.rs`
module-declaration, enum-variant, and match-arm sites — never as inline `cfg`
inside logic files. `syn`/`proc-macro2` stay non-optional because `common`
(`parse`, `exclude`) uses them unconditionally, so the features gate the check
modules rather than the AST stack itself.

## Public API surface

`common` is intentionally public so a consumer can build custom checks on the
shared walker / violation / baseline / report / parse infrastructure. Keep
additions to it deliberate and documented; internal helpers stay `pub(crate)`.
