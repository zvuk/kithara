<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-devtools.svg)](https://crates.io/crates/kithara-devtools)
[![docs.rs](https://docs.rs/kithara-devtools/badge.svg)](https://docs.rs/kithara-devtools)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-devtools

Reusable, config-driven command core for `cargo xtask` build tooling. It holds
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
- `typos`, `similarity`, `ast-grep` — thin wrappers over the matching CLIs with
  the workspace config pinned.
- `manifest`, `orphans` — Cargo manifest hygiene and per-package orphan checks.
- `test` — workspace tests through `cargo nextest` with lane / backend / feature
  selection.
- `health` — aggregated workspace health report.
- `quality` — rstest / unimock / trait-mock audits.
- `scope` — translate scope tokens to tool-specific flags.
- `perf-compare` — compare hotpath timing tables against a baseline.
- `perf` — test-suite performance pipeline: matrix, slow aggregation, samply
  profiling, merged report, and xctrace escalation.
- `viz` — architecture visualization. *(feature `viz`)*

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

## Features

- `lint` (default) — the syn-based `arch`/`style`/`idioms` lint family.
- `viz` (default) — architecture visualization.

Both are on by default; `--no-default-features` drops those command families for
a project that only wants format/test/health and friends.
