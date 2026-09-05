<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-devtools.svg)](https://crates.io/crates/kithara-devtools)
[![docs.rs](https://docs.rs/kithara-devtools/badge.svg)](https://docs.rs/kithara-devtools)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-devtools

Reusable, config-driven command core for cached xtask build tooling, invoked
through `just tooling xtask`. It holds the project-agnostic commands so several
workspaces can share one implementation and keep only their own project-specific
commands in a thin `xtask` binary.

Contracts and invariants live in [`CONTEXT.md`](CONTEXT.md); this file is the
overview.

## Usage

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

### Configuration

Everything project-specific comes from `.config/xtask.toml`, parsed once into
the context. The file is optional: a project with none gets documented code
defaults. A project puts its own sections under `[ext.*]`, which the core passes
through untouched, and registers only its own heavyweight commands as deep
stages. External programs are addressed by role under `[tools.<role>]`, so a
project that installs them elsewhere sets a path instead of patching the core.
Quality Lab intentionally keeps a separate, **required**
`.config/quality-lab.toml` so heavyweight analyzers never reach the fast lint
path. The ownership rules, the schema's strictness, and what each section is
allowed to declare are in [`CONTEXT.md`](CONTEXT.md).

## Key Types

`CoreCommand` groups the shared commands into a few families:

- **Hygiene** — formatting, typo and ast-grep wrappers, Cargo manifest checks,
  per-package orphan detection, and the ratcheted `arch` / `style` / `idioms`
  lint namespaces. *(feature `lint`)*
- **Analysis** — recursive Rust type-shape and behavior similarity, workspace
  health, public-surface comparison, feature-powerset checking, and the
  deterministic quality assessment plus the opt-in Quality Lab for heavyweight
  external analyzers.
- **Execution** — workspace tests, the test-suite performance pipeline and its
  baseline comparison, and repeated-test evidence runs with independent
  verification of the downloaded artifact.
- **Reporting** — LOD-controlled Mermaid architecture views built from source,
  runtime, and semantic evidence *(feature `viz`)*, and consolidation of one CI
  run's archived quality artifacts into a single markdown report.

Command names, flags, and output contracts are owned by the root `justfile`,
`.config/just/`, and
[`../../docs/guides/tooling.md`](../../docs/guides/tooling.md), not by this
crate's docs; `just tooling xtask --help` lists the current surface.

## Features

- `lint` (default) — the syn-based `arch` / `style` / `idioms` lint family.
- `viz` (default) — architecture visualization.

`--no-default-features` drops those command families for a project that only
wants format, test, health, and friends.
