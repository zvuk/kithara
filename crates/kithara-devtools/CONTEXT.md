# kithara-devtools — contracts

Owning-crate contracts for the reusable, project-agnostic xtask command core.
[`README.md`](README.md) is the overview. Command names, flags, and output
contracts belong to the root `justfile`, `.config/just/`, and
[`../../docs/guides/tooling.md`](../../docs/guides/tooling.md); repo-wide rules
belong to [`../../AGENTS.md`](../../AGENTS.md). This file owns only what neither
the code nor a test already states.

## Ctx lifecycle

`Ctx::load` and `Ctx::load_from_manifest` resolve the workspace root once via
`cargo metadata --no-deps`, parse the project and similarity configs, and retain
the metadata. `Ctx::new` builds a metadata-less context: a command reaching
`ctx.metadata()` there gets a typed error instead of a second shell-out to cargo.
Commands take `&Ctx` and must not re-resolve the root or re-parse config. The one
sanctioned exception is `common::walker` — the scoped walkers reload
`ProjectConfig` themselves because the lint namespaces run without a `Ctx`.

## Configuration contract

`.config/xtask.toml` is optional: a missing file yields documented code defaults,
not a fallback chain, and `init` scaffolds it plus empty per-namespace baselines.
`project.name` is derived from cargo metadata when absent; it is a human-facing
label (report titles, temp-dir prefixes), so that derivation is a sanctioned
user-facing default rather than state resolution.

**`[ext]` ownership rule.** The core schema names generic concerns only. A project
may fill those generic shapes with its own values, but a concern needing
project-specific keys lives under `[ext.*]` — a raw table the core never
interprets and the consuming bin deserializes into its own typed view (kithara:
`xtask/src/config.rs`). Every core section is `deny_unknown_fields`, so a consumer
section at top level is a typed parse error naming the token.
`tests/config_contract.rs` pins the defaults, the rejection, the passthrough, and
the requirement that a stress config name the directory it builds into.

`[workspace-scan] exclude` globs apply in the scoped walkers only; raw `.rs`
discovery stays a pure directory walk.

The nextest profile named by `[perf]` must expose a JUnit path — the perf matrix
copies that report out of the target directory into the run data — and a lane
without one is skipped and fails the command.

`[quality.assessment]` declares only project-specific deep-stage execution: no
source relationships and no metric values, because delegated commands keep their
own budgets and output schemas.

## CLI surface, lint baselines, and exclusion

`CoreCommand` is flattened into the consumer's own subcommand enum and dispatched
by `run`. Command names, flags, and help text are the public surface — treat a
change as an API change for the consuming project.

`audit-clippy` keeps workspace selection unless an explicit package selector
replaces it, and `--fix` applies machine-applicable suggestions over that same
scope before re-reporting; a failed fix stops before reporting and preserves
cargo's exit status. Both passes disable the workstation sccache wrapper and keep
incremental builds, matching the repository's Clippy cache contract. Pinned in
`audit_clippy.rs`.

`Baseline` compares on a line-insensitive canonical key, so reformatting never
re-fingerprints an unchanged violation, while a positional-only key keeps its line
as its sole handle. A violation with no baseline entry fails only at
`Severity::Deny`. Pinned in `common/baseline.rs`.

Exclusion runs in three complementary passes — path globs, AST `#[cfg(test)]`
ranges (only `test`-keyed cfgs; a feature cfg is untouched), and inline-module
globs. An unparseable file contributes no ranges, so its violations are kept
rather than silently dropped, and `scan_all_rules` names ast-grep rule IDs that
re-run over the full tree, tests included. Pinned in `common/exclude.rs`.

`thin_wrapper_economy` owns synchronous one-expression free helpers, and its
threshold is net savings after paying for the definition. Even a scoped run scans
the whole workspace before filtering findings, so omitted callers cannot make a
metric look exact, and ambiguous symbols get no invented savings. The check is
diagnostic-only and must stay so: a syntax-only rewrite cannot preserve trait
lookup, never coercion, or `#[track_caller]`, so an autofix needs compiler-backed
callee resolution. Attributed helpers are excluded because an attribute can change
call or code-generation semantics; multi-statement logging proxies belong to the
ast-grep rule.

## Architecture visualization (`viz`)

One lossless source-evidence graph is built, then scope and level of detail
project it into artifacts below `target/architecture/<revision>/`. View and crate
selectors are projections of that graph, not independent analyzers: a crate scope
needs no runtime scenario and no extra discovery command, and never expands
external packages. Static call targets stay candidates until rust-analyzer
resolves them.

LOD is independent from scope. Concrete types own their `impl` methods, traits are
contracts, and free functions belong to one module-functions abstraction. Page
selection is navigation, not evidence truncation: every source contour stays in
`contours.json`, there is no diagram node budget, and the manifest records
complete visible-node coverage. An endpoint hidden by LOD lifts to its nearest
visible owner; equal visible endpoint/kind pairs aggregate while retaining the
original evidence, and relation kinds stay distinct. Metrics come from the same
contracted relations as the diagram, and the aggregate index is diagnostic and
alters no CI budget.

Project defaults and the repeatable CLI filters compile into one additive
projection filter: it removes matching symbols before semantic selection and
matching contours plus incident edges from the `DiagramModel`, never alters raw
evidence, and never disables an excluded package used as a runtime scenario.
A module pattern matching any ancestor of a canonical `package::module` path
excludes the descendant, and relations never lift through an excluded endpoint. An
emptied projection is an error, not an empty diagram.

Configured runtime scenarios are the only project-specific runtime evidence input.
The schema is strict and tagged — Cargo tests, Cargo binaries, or existing trace
paths — and targets are checked against Cargo metadata, then launched with
structured arguments, a bounded timeout, captured logs, and
`ARCHITECTURE_TRACE_PATH`. No shell command is ever stored in config. Producers
write through the public, domain-neutral `viz::trace` JSONL API; a cross-thread
send connects only through an explicit correlation identifier. Source matching
enriches existing syntax nodes, and an unmatched record stays a visible runtime
event rather than a guessed static target. A manually supplied trace carries the
`Manual` evidence class. Runtime enrichment precedes semantic resolution, so a
selected scenario limits rust-analyzer work to the functions the trace observed.
The Markdown report derives from the visible `DiagramModel`, so every finding
points at a visible contour.

Missing, timed-out, or failed semantic resolution yields the same static
classification, cause kept in diagnostics. Degraded runtime observation cannot
invalidate the static projection: optional degradation warns and succeeds, while
explicitly required semantic, scenario, or trace evidence errors after preserving
the artifacts.
Truncation is explicit, applies only to evidence collection, and never removes
nodes because of diagram size.

## Health stage provisioning

Each stage runs a tool pinned and installed by `.config/ci-pins.toml` and
`xtask/src/ci/image.rs`. `ENV_SKIP_MARKERS` lets a stage whose provisioned tool is
transiently missing read as SKIP rather than a false FAIL, and `.strict()`
withholds that pass from a stage whose tool the fleet never provisions. Stage
invocations and their rationale live in `health.rs` and are pinned by its tests.
What is not visible there:

- `semver-checks` compares against a git baseline, so CI has to fetch it.
  `actions/checkout` brings one commit of one branch; without an explicit fetch
  the stage dies on `couldn't parse revision`. The standalone lane in `semver.rs`
  reads the baseline lockfile first and names any configured package that has no
  earlier surface to compare.
- `geiger` stays `.advisory()` because cargo-geiger exits non-zero whenever it
  emits a warning, and here it always does: it cannot match the workspace's own
  path packages.
- `lockbud-deadlock` is a rustc driver, not a crates.io package, so it has no
  version pin: the image installs it from git and exports the nightly it was built
  against as `KITHARA_LOCKBUD_TOOLCHAIN`, read by the stage and both recipes. Its
  `-l` and `-b` crate filters do not restrict what it reports — on the pinned
  commit all three flag forms print the same findings across workspace and
  dependency crates — so the verdict parses the per-crate summary lines and counts
  only workspace members, spelled as a compiled crate is (underscores, not
  hyphens).
- `machete` is handed the directories to walk because cargo-machete 0.9 takes no
  exclude flag and otherwise walks the whole tree. The list comes from cargo
  metadata, so a new crate is covered without being named anywhere.
- `workspace-unused-pub` needs the `rust-analyzer` rustup *component*. rustup ships
  the proxy binary either way, so an image without the component does not report a
  missing tool — it reports a non-zero exit. `docker/ci.Dockerfile` adds the
  component for that reason alone.

## Stress run ownership

`stress run` is the sole portable lifecycle owner for repeated-test evidence,
writing one fresh raw directory per run. The project's `[stress]` section solely
owns modes, test features, child environment, paths, limits, and evidence markers;
devtools applies that policy without embedding product feature or environment
names. The manifest freezes the resolved test runner, its arguments, and effective
features so the independent reporter can reject controller or config drift. The
inventory-by-iteration contract, not nextest's last iteration status, owns the
primary verdict.

A run owns the directory it builds into. `[stress].build_dir` names it relative to
the checkout a lane compiles, and the run exports it as `CARGO_TARGET_DIR` to
every child *after* the lane's own environment, so no mode can name it away; a
relative value is refused before a child inherits it. An inherited target
directory is shared with everything else on the host, and a stress run lasts hours:
binaries cleared mid-run turn every remaining repeat into a millisecond exec
failure, and the lane then reports nothing about the revision it was asked about.
The price is one cold build per run per tree. The manifest records the resolved
directory as an observation, not provenance — the reporter has no such directory
and is never asked to agree about one.

Report anchors are the exception — they stay on the checkout that runs the tests,
because nextest's store is rooted at the workspace root and does not follow
`CARGO_TARGET_DIR`. An anchor placed under the build directory reads a path
nextest never writes, and one run lost all six lanes' evidence to exactly that.

The lane holds a lease on the build directory for as long as it owns it: a shared
claim on `.kithara-job-lease` there, which a build-cache budget elsewhere must
take exclusively before reclaiming — the one request a shared holder refuses.
Exporting the directory to the children cannot stand in for it: the children are
cargo, which claims nothing, and a directory no budget can see is one no budget
can reclaim.

Linux runner targets cross the Colima bind mount, where file locks do not reach
the macOS host. The public CI command therefore refreshes
`.kithara-job-heartbeat` beside the lock, and host cleanup accepts only a recent
heartbeat. Normal exit removes it; a killed job becomes reclaimable after the
bounded freshness window.

Pressure sampling ends after the test and evidence phase so the reporter consumes
a closed stream, and its end marker records the *primary* exit status, while the
manifest's exit code is the later combined verdict and can additionally reflect
staging or supplemental-evidence errors.

`stress report` independently consumes an uploaded raw directory: it compares the
manifest against trusted checkout and workflow inputs, checks that pressure
sampling ended healthy, correlates configured evidence by exact nextest attempt,
and returns non-zero for failed, missing, partial, duplicate, malformed, or
mismatched evidence. GitHub Actions owns only authorization, immutable checkout
selection, job isolation, artifact transfer, and publishing the rendered summary.

A lane is barred from the cross-lane comparison only by what its JUnit report
cannot account for: a missing iteration, a quarantined repeat, a selected test the
report never names. A partial evidence overlay - a census past the line pass's
record bound, a lost output tail, an absent envelope directory - is a diagnosis
caveat and stays one, because the counts, the rates and the verdict are read from
the JUnit report alone. The reason an excluded lane carries is the sentence that
lane recorded, never a cause re-derived at the summary.

## Quality assessment contract

`quality assess` is an artifact federation layer: the lint, architecture,
similarity, health, Quality Lab, test, dependency, concurrency, performance, and
platform commands remain the canonical owners, and the assessment normalizes and
correlates their output while reimplementing none of their metrics.

The complete profile disables project-default architecture and similarity
exclusions and pulls in integration tests plus test and tooling crates; an
explicitly selected crate or canonical `package::module`
scope is included even when defaults exclude it. Standard depth executes each
portable gate separately, so stage evidence is attributable without paying for the
heavyweight sections of `health`; deep runs the full health pipeline plus the
registered rare stages. A configured stage is advisory unless it records an
already-established project gate.

Artifacts land under a revision-and-profile directory. A dirty worktree gets a
content-digest suffix and records that digest: committed Quality Lab output, Cha's
especially, must not claim coverage of dirty content. Reuse rebuilds the report
from stage artifacts already on disk and rejects malformed stage evidence.

The workspace debt target is zero and the refactor threshold is 100; a smaller
scope scales that by LOC and never reaches zero. Existing lint baseline entries
count as debt, and baseline growth is a regression. A hard invariant, debt at or
above threshold, debt regression against a baseline, or same-location
corroboration by two independent tools yields `refactor`; otherwise diagnostic
findings yield `investigate`, remaining debt `stable-with-debt`, uncovered signals
`evidence-gap`, and a clean run `healthy`. Verdicts are advisory and do not fail a
complete command; a broken stage marks the analysis partial, while invalid input
or broken required analysis preserves a partial artifact and returns an error.

The tool coverage matrix must account for every known signal. A tool the project
declares not-applicable stays visible in the matrix with its reason and can never
be scheduled as a deep stage.

## Behavioral similarity ownership

`similarity` owns native source-level comparison of Rust abstractions: it parses
the selected production sources, writes its own revision-scoped artifacts, then
runs the external similarity-rs function-copy profile. Native
findings are diagnostic and do not change the thresholds that profile owns. Only
the strict profile includes test paths and `#[cfg(test)]` items; the other two
keep the production-only policy in both passes.

Built-in type families carry conservative similarity degrees; a dependency family
activates only when Cargo metadata shows the dependency, and
`.config/similarity.toml` may add project families of two or more members, or
directional pair relations with substitution caveats. Generic and local names are
erased where they carry no semantics; domain types, constructors, significant
macro symbols, and effects remain. An `impl` block in a separate file attaches
only when its owner resolves uniquely in the workspace or, failing that, uniquely
within the owning crate. Partial state overlap without matching behavior is a review finding;
composition is recommended only when aligned `impl` behavior supports it.

The JSON report and graph are exhaustive; the Mermaid view aggregates candidates by
crate pair, so rendering stays useful without a node or finding limit. The manifest
records the exact roots and whether project-default exclusions were disabled, so an
assessment never reuses evidence from another profile or scope. Proc-macro output
is not expanded, similarity never proves substitutability, and the caveats must be
checked before refactoring.

## Quality Lab ownership

`quality lab` owns heavyweight external analysis that must stay outside the fast
lint path, the normal audit, and pre-commit. Its `.config/quality-lab.toml` is
**required**, loaded exactly once, and carries a strict versioned schema
independent of `.config/xtask.toml`. Every external tool version must match its pin
before analysis runs.

- The coverage profile owns the cargo-crap coverage-risk gate. A production run
  without a baseline emits the absolute artifact; a pull-request run supplies it
  and gates regressed entries plus new high-risk functions. The wrapper judges the
  delta JSON rather than the tool's own regression exit code, which does not cover
  new functions. A failing instrumented test run still writes its coverage reports
  and still runs cargo-crap: the combined stage preserves the test exit as findings
  instead of losing the risk evidence. The rendering carries no verdict of its own
  and never takes the baseline, because a delta narrows what the gate accepts while
  the report states the whole picture.
- The scheduled profile's findings are advisory; a missing tool, invalid report,
  version mismatch, or timeout is a tool error.
- The manual profile adds a read-only repo score. A missing executable is skipped
  and findings stay advisory. A direct non-coverage tool run follows this policy.
- Cha runs only from a clean, non-shallow worktree and analyzes a disposable local
  clone whose revision is verified against HEAD. The clone is deleted afterwards so
  Cha cache state cannot leak into the source checkout.

## Orphan sweep ownership

An orphan is a file no `mod` declaration in its package names. `cargo modules
orphans` answers a narrower question — what one resolved configuration loads — and
pairs a file with its parent by directory convention, so a module behind an unset
cfg, or one reached through `#[path]` from a sibling, reads as unreferenced to it.
The sweep therefore treats its findings as candidates and settles each against the
source: `declared.rs` walks the package tree, resolves `#[path]` and `cfg_attr`
paths against the directory of the declaring file and plain `mod` declarations
against the directory that file owns, and drops any candidate the source names.
Drops are printed per package, never silent — that filter is the reason the sweep
can be green at all.

The tool selects one target per run and offers no selector beyond library and
binary, so the sweep enumerates both for every package and folds a package's
targets into one verdict: a file one target reports and another declares is not an
orphan. That is what lets the exclude list stay empty — a package without a library
is swept through its binaries instead of dropped.

One run loads the whole workspace into a rust-analyzer database and peaked at
3.0 GiB here, so concurrency is a property of the job rather than a constant: the
sweep takes the smaller of the cores it may use and its cgroup memory cap divided
by that budget, capped at four. A CI job container bounded at 8 GiB and three cores
exhausted its cgroup under a fixed four, and the kernel killed the step before any
verdict. The chosen count and the numbers behind it are printed, because a sweep
quietly running one at a time is otherwise indistinguishable from a slow one.
Without an explicit deny the run is advisory.

## CI report ownership

`ci-report` consolidates one CI run's archived quality artifacts into a single
markdown document. It reads artifacts, never tools, so it cannot disagree with what
a job measured, and locates inputs by file name rather than by an upload's
directory layout. A section whose input never arrived says so — an omitted section
would read as "nothing to report" from a run that reported nothing. Long tables are
carried as a capped prefix and the omission is stated, because a step summary has a
size limit.

Promoting a unique external check into `syn`, Cargo metadata, git, or ast-grep
requires repeated actionable, deterministic evidence and two comparison runs before
the adapter is retired.

## Feature gating and public API surface

`lint` and `viz` are default-on cargo features gating the syn-heavy command
modules. Gates live only at the `lib.rs` module-declaration, enum-variant, and
match-arm sites — never as an inline cfg inside a logic file. `syn` and
`proc-macro2` stay non-optional because `common` uses them unconditionally; `lint`
additionally turns on the optional `quote` dependency. The features gate the check
modules, not the AST stack.

`common` is intentionally public so a consumer can build custom checks on the
shared infrastructure; internal helpers stay `pub(crate)`, as do all `viz` modules
except `viz::trace`.
