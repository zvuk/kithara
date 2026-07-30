# Tooling Policy

Use this when touching repo tooling, formatter/lint config, or dependency audit
policy. Keep `AGENTS.md` short; put command details here.

## Fast Gate

- `just fmt check`: Rust fmt, Cargo manifest dependency order (`kithara-*`
  first), formatted non-Cargo TOML, and sorted JSON/JSONC.
- `just check clippy`: workspace Clippy with warnings denied.
- `just lint ast-grep`: structural policy rules from `.config/ast-grep/`.
- `just lint arch`: fast architecture gate used by pre-commit.

These are suitable for local pre-commit feedback.

## Architecture Analysis

- `just arch viz` automatically collects the workspace source graph, runs all
  configured runtime scenarios, asks rust-analyzer to resolve selected calls,
  and writes the Mermaid diagram, linked crate/hotspot-subsystem pages, and
  graph-derived complexity report below `target/architecture/<revision>/`.
- Scope and detail are independent. Exact common commands are:
  `just arch viz --lod 0` for crates; `just arch viz --crate <package>` for
  automatic LOD 1 subsystems; `just arch viz --crate <package> --module <path>`
  for automatic LOD 2 abstractions; `just arch viz --crate <package> --lod 3`
  for constructors, boundary methods, resources, messages, and tasks; and
  `just arch viz --crate <package> --module <path> --lod 4` for the complete
  focused call graph. A crate scope hides Cargo dependencies and incoming
  callers while keeping concrete outgoing public interactions as compact
  external ports.
- `--view hierarchy|ownership` changes only the projection. `--semantic
  off|required`, `--runtime off`, `--scenario <name>`, and `--trace <jsonl>`
  control evidence collection.
- `[architecture.filters]` supplies project-default crate/module exclusions.
  Repeat `--exclude-crate <glob>` or `--exclude-module <glob>` for additive
  one-off exclusions. Excluded runtime-test packages may still produce
  evidence, but they do not enter semantic selection, the diagram,
  `projection.json`, findings, or architecture counters. `manifest.json`
  records the effective filters and excluded counts.
- `metrics.json` contains resolved-static and candidate profiles for the
  selected scope and every generated contour. It reports coupling, cohesion,
  propagation, cycles, depth, bottlenecks, ownership, boundary alignment,
  abstractness, and the experimental ACI. Runtime evidence is an overlay and
  cannot change the stable score. Boundary concentration and external coupling
  enter the ACI in proportion to actual boundary load. The ACI is diagnostic,
  not a CI budget.
- There is no diagram node budget. Hidden methods are lifted to their visible
  abstraction with count, method pairs, origins, and evidence retained in
  `projection.json`. LOD 4 writes an index plus linked contour pages instead of
  dropping nodes. Optional and target-gated Cargo edges use the distinct
  `conditional` evidence style.
- Read `manifest.json` before using a result as architecture evidence.
  `complete` and `runtime-enriched` are successful observations; `truncated`
  names an evidence-collection limit, never diagram node removal;
  `static-only` has no runtime evidence; `incomplete` preserves partial files
  but is not an acceptance result.
- Runtime traces and scenario stdout/stderr stay beside the diagram. Trace
  absence never proves a path is dead, and unresolved calls are never assigned
  a guessed target.

## Full Audit

- `just ci audit`: scoped Rust fmt, Clippy, ast-grep, xtask lint, typos,
  similarity, and scoped orphan-module checks. With no scope, the orphan stage
  is latency-capped; `just ci health` owns the full workspace orphan sweep.
- `just lint full`: fast lint plus xtask self-tests and quality scans.
- `just ci health`: broad local health report; heavy or environment-sensitive
  stages may report SKIP.
- Audit and health consume one canonical argv source for their shared stages and
  validate each xtask command shape in the `kithara-devtools` unit tests.

## Decision-Oriented Assessment

- `just quality assess` rebuilds the standard product evidence and writes
  `manifest.json`, `assessment.json`, and `assessment.md` below
  `target/quality-assessment/<revision>/product-standard/`.
- Standard runs the portable format, compiler/lint, quality, unused-public,
  test, similarity, and architecture stages independently. Full `health` and
  dependency/API/security sweeps belong to `--depth deep`.
- `--profile complete` includes integration tests, test/tooling crates, xtask,
  devtools, and other project-default exclusions. `--depth deep` runs the
  configured heavyweight analyzers and project/platform scenarios.
- Scope with `--crate <package>` or
  `--module <package>::<module-path>`. Compare with an earlier report using
  `--baseline <assessment.json-or-directory>`. `--reuse-existing` deliberately
  skips refresh and only federates compatible existing artifacts.
- A global unversioned report is not treated as reusable evidence merely
  because the file exists. Fresh stages must create or update their declared
  artifacts; otherwise the assessment is `partial`.
- Start with the assessment manifest. `partial` means at least one required
  stage is broken; preserved logs are evidence of the gap, not a complete
  result. A `refactor` verdict is advisory and still exits successfully when
  analysis is complete.
- Baseline entries are debt. The target is zero, the workspace refactor
  threshold is 100, and smaller scopes use the LOC-proportional threshold
  recorded in the report. ACI is diagnostic; use it to rank contours and seek
  corroboration rather than inventing a score gate.
- For source-aware synthesis and deep-report behavior, use
  `docs/skills/quality-assessment/SKILL.md`.

## Similarity Analysis

- `just lint similarity` automatically combines native abstraction analysis
  with the established `similarity-rs` function-copy scan. Append one or more
  crate `src/` paths for a focused run; no discovery or secondary visualization
  command is required.
- Native artifacts live under `target/similarity/<revision>/`. Start with
  `report.md`: its Mermaid graph aggregates every candidate by crate pair and
  its findings explain state, behavior, matched fields, type-family scores,
  substitution direction, and caveats. `report.json` and `graph.json` retain
  exhaustive unaggregated evidence; `manifest.json` records the profile, scan
  size, candidate count, and cache use.
- `.config/similarity.toml` owns project exclusions and optional type
  dictionaries. `[[types.relations]]` gives a pair a `similarity` in
  `[0.0, 1.0]`, `substitution` (`safe`, `conditional`, or `incompatible`),
  `direction` (`bidirectional`, `left-to-right`, or `right-to-left`), and
  `caveats`. `[types.families.<name>]` supplies `members` and
  `default_similarity`.
- Audit and advisory analyze production source; strict also includes test paths
  and `#[cfg(test)]` items. Native analysis is diagnostic and does not alter
  existing CI thresholds or latency budgets. A high score is a refactoring
  candidate, never proof of behavioral equivalence.

## Dependency And Surface Tools

- `cargo-deny`: licenses, bans, advisories, and source policy.
- `cargo-machete`: unused dependency smoke test.
- `cargo-shear`: unused, misplaced, and unlinked dependency/file audit. Treat
  new findings as dependency-boundary debt; ignore only with documented metadata.
- `cargo-hack`: feature-powerset compatibility.
- `cargo-semver-checks`: release-facing public API compatibility.
- `cargo-public-api`: manual public surface listing/diff for planned API
  changes; use one package at a time.
- `cargo-geiger`: unsafe inventory. It is evidence for audit, not a security
  verdict by itself.
- Dylint or Semgrep: add only for rule classes that ast-grep and xtask cannot
  express cleanly. Do not create a second custom-rule stack for existing rules.

## Formatting Ownership

- `just fmt` is the formatter harness and `just fmt check` is its gate. Use
  `just tooling xtask format --only rust|manifest|toml|json|markdown` only for
  scoped formatter work.
- `rustfmt.toml` owns `.rs` formatting.
- `.config/tomlfmt.toml` plus `cargo-sort` provide the mechanical `Cargo.toml`
  write pass. `just deps manifest dependency-order` owns the gate: internal
  `kithara` / `kithara-*` dependencies stay above external crates, and each
  dependency group stays sorted by key.
- Do not use `cargo sort --check` in gates: it conflicts with the repo's
  internal-first dependency policy after the post-pass.
- `taplo` owns non-Cargo TOML formatting.
- `tidy-json` owns JSON/JSONC sorting and formatting.
- `mdfmt` owns Markdown formatting. It is an explicit recipe/advisory health
  signal until the historical Markdown tree is cleaned up.
- Do not add a second formatter for the same file class unless the owner is
  changed here and in `.config/just/fmt.just`.
