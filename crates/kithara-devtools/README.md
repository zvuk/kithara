<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-devtools.svg)](https://crates.io/crates/kithara-devtools)
[![docs.rs](https://docs.rs/kithara-devtools/badge.svg)](https://docs.rs/kithara-devtools)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-devtools

Reusable, config-driven command core for cached xtask build tooling invoked
through `just tooling xtask`. It holds
the project-agnostic commands so several workspaces can share one implementation
and keep only their own project-specific commands in a thin `xtask` binary.

Contracts and invariants live in [`CONTEXT.md`](CONTEXT.md); this file is the
overview.

## Commands

Exposed through the `CoreCommand` subcommand enum:

- `init` — scaffold the workspace tooling config and lint baselines.
- `lint` — architectural / style / idiomatic fitness functions (`arch`, `style`,
  `idioms`), ratcheted against a baseline. *(feature `lint`)*
- `format` — Rust, Cargo manifests, TOML, JSON, and Markdown formatting.
- `typos`, `ast-grep` — thin wrappers over the matching CLIs with the workspace
  config pinned.
- `similarity` — recursive Rust type-shape and normalized behavior analysis
  with explainable revision-scoped artifacts, followed by the transitional
  `similarity-rs` function-copy scan.
- `manifest`, `orphans` — Cargo manifest hygiene and per-package orphan checks.
- `test` — workspace tests through `cargo nextest` with lane / backend / feature
  selection.
- `health` — aggregated workspace health report.
- `quality` — deterministic workspace/crate/module assessment, rstest / unimock
  / trait-mock audits, and the opt-in Quality Lab for heavyweight external
  analyzers.
- `scope` — translate scope tokens to tool-specific flags.
- `perf-compare` — compare hotpath timing tables against a baseline.
- `perf` — test-suite performance pipeline: matrix, slow aggregation, samply
  profiling, merged report, and xctrace escalation.
- `viz` — LOD-controlled Mermaid architecture diagrams from source evidence,
  written below `target/architecture/<revision>/`. Configured runtime scenarios
  and rust-analyzer semantic evidence enrich the same graph automatically.
  A workspace run links crate and hotspot-subsystem diagrams and writes explainable
  per-contour complexity metrics derived from the same contracted graph.
  `--crate <package>` isolates the selected crate, hides Cargo dependencies and
  incoming callers, and renders concrete outgoing interactions as compact
  public external ports. Automatic detail is crates for a workspace, top-level
  subsystems for a crate, and abstractions for a module.
  `--lod auto|0|1|2|3|4` moves from crates through subsystems, modules, and
  abstractions to the complete call graph. Repeatable
  `--exclude-crate <glob>` and
  `--exclude-module <glob>` filters remove non-product contours from the
  projection and report.
  *(feature `viz`)*

Without `--crate`, LOD 1 keeps the workspace Mermaid at crate level and writes
subsystem views as linked per-crate pages; it never expands every workspace
module into one diagram.

## Consuming it

Add the dependency and flatten `CoreCommand` into your own bin's subcommand
enum, keeping your project-specific commands alongside it:

```rust
#[derive(clap::Subcommand)]
enum Command {
    #[command(flatten)]
    Core(kithara_devtools::CoreCommand),
    // ... your project-specific commands
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let ctx = kithara_devtools::Ctx::load()?;
    match cli.command {
        Command::Core(cmd) => kithara_devtools::run(&cmd, &ctx),
        // ... your arms
    }
}
```

## Configuration

Everything project-specific comes from `.config/xtask.toml`, parsed once into
`Ctx::config`. The file is optional: a project with none gets documented code
defaults, and `project.name` is derived from cargo metadata. Unknown top-level
sections are a typed error (`deny_unknown_fields`); a project puts its own
sections under `[ext.*]`, which the core passes through untouched.

The shared `[workspace-scan] exclude` globs drop directories (media trees,
virtualenvs, …) from the scanning commands.

`quality assess` federates the canonical architecture, similarity, health,
lint-baseline, test, dependency, Quality Lab, performance, concurrency, and
platform artifacts. The default `product` / `standard` run rebuilds the
portable core evidence. `--profile complete` includes source surfaces excluded
by project defaults; `--depth deep` also runs the project-configured rare
stages. `--crate` and canonical `--module package::path` scopes reuse the same
policy with a LOC-proportional debt threshold. Artifacts live below
`target/quality-assessment/<revision>/<profile>-<depth>/`.

Projects register only their own heavyweight commands under
`[[quality.assessment.deep_stages]]`; the core supplies orchestration, logs,
status, normalization, and coverage accounting:

```toml
[[quality.assessment.deep_stages]]
name = "binary-bloat"
command = ["cargo", "bloat", "--release"]
tools = ["cargo-bloat"]
hard_invariant = false
complete_only = true
```

Tools deliberately excluded by project policy remain visible in the coverage
matrix without being executed:

```toml
[[quality.assessment.not_applicable_tools]]
tool = "cargo-mutants"
reason = "Workspace-wide mutation testing is not actionable for this project."
```

`platforms` optionally restricts a stage to `std::env::consts::OS`.
`{revision}` and `{short_revision}` placeholders are expanded in commands and
expected artifact paths. Set `hard_invariant = true` only for an existing gate
whose failure already requires action; advisory analyzers keep the default
`false`. Existing tool budgets remain owned by the delegated commands.

`[architecture.filters]` provides additive project defaults for the same
repeatable CLI filters:

```toml
[architecture.filters]
exclude_crates = ["integration-tests", "xtask"]
exclude_modules = ["*::tests"]
```

Crate patterns match Cargo package names. Module patterns match canonical
`package::module` names and exclude the complete matched subtree. Excluded
packages may still run as runtime evidence producers; they do not enter
semantic selection, Mermaid, `projection.json`, findings, or architecture
counters. The complete diagnostic evidence remains in `graph.json`, while
manifest schema v4 records the effective filters, excluded node/edge counts,
hierarchical pages, and machine-readable artifact paths.

`[architecture.runtime]` can declare portable tests, binaries, or existing
JSONL traces that exercise representative flows:

```toml
[[architecture.runtime.scenarios]]
name = "queue-playback"
command = "test"
package = "my-integration-tests"
test = "architecture"
filter = "queue_playback"
ignored = true
timeout_secs = 120
```

The test or binary receives `ARCHITECTURE_TRACE_PATH`. It can write neutral
records through `viz::trace::{TraceRecord, TraceRecordKind, TraceSource,
TraceWriter}`; no Kithara domain type or macro is required. `just arch viz`
runs every configured scenario. `just arch viz --scenario queue-playback`
runs and projects one flow, while `--trace <path>` merges an existing trace as
manual evidence. `--semantic off|required` and `--runtime off` control optional
enrichment without changing the artifact layout.

The crate selector is a projection over the full workspace graph, not a
package-only scan. It omits inter-crate Cargo `depends on` relations and
incoming workspace callers. Concrete outgoing cross-package interactions end
at compact ports labelled with the public target symbol; the target crate is
not expanded. Repeated relations retain their count, origins, evidence style,
and method pairs.

The canonical graph groups concrete types and their methods, trait contracts,
and each module's free functions. Hidden method relations lift to the nearest
visible contour without merging calls with ownership, messaging, transfer, or
spawn relations. Optional and target-gated Cargo dependencies are styled as
conditional rather than required.

Each run writes `architecture.md`, `architecture.mmd`, `metrics.json`,
`contours.json`, `graph.json`, `projection.json`, and `manifest.json`.
Workspace runs also write `workspace.mmd`, readable diagrams below `crates/`,
and linked hotspot-subsystem reports. A focused crate run writes every
subsystem report. `metrics.json` separates resolved-static and
candidate profiles, retains per-contour profiles, and labels the aggregate ACI
experimental. Boundary concentration and external coupling contribute in
proportion to the actual boundary load, so a single edge cannot dominate a
large sparse contour. Runtime relations are a separate overlay and cannot
change the stable index. Runtime traces and captured process logs are preserved in
adjacent `traces/` and `logs/` directories. There is no diagram node cap. LOD 4
writes an index plus linked documents below `contours/`, with manifest coverage
proving that partitioning did not remove nodes. The manifest status is
`complete`, `truncated`, `static-only`, `runtime-enriched`, or `incomplete`;
`truncated` applies to evidence collection, not diagram size. An incomplete run
returns an error after preserving partial artifacts.

`[perf]` configures the generic test-suite performance pipeline:

- `lanes` is the matrix of `{ flash, backend }` combinations to measure.
- `primary_lane` is the lane used for ranking/profile defaults; an empty value
  means the first configured lane.
- `frame_prefix` overrides gecko profile frame attribution; if omitted, the
  project name is used.
- `nextest_profile` names the nextest profile used by `perf matrix`; it defaults
  to `perf`.

The selected nextest profile must write JUnit at `junit.xml`, for example:

```toml
[profile.perf.junit]
path = "junit.xml"
```

Quality Lab intentionally has a separate, required
`.config/quality-lab.toml`. It pins analyzer versions, tool/profile time
budgets, and the output directory without adding heavyweight tools to the fast
lint path. Use `quality lab list` to inspect policy and `quality lab run
coverage|scheduled|manual|<tool>` to execute it.

## Features

- `lint` (default) — the syn-based `arch`/`style`/`idioms` lint family.
- `viz` (default) — architecture visualization.

Both are on by default; `--no-default-features` drops those command families for
a project that only wants format/test/health and friends.
