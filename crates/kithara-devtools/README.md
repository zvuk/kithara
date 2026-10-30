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

Contracts and invariants live in [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-devtools); this file is the
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

For a selected workspace in another directory, load its manifest with
`Ctx::load_from_manifest(&manifest_path)` and pass that context to `run`.
The lint, manifest, Clippy, test and health commands use its Cargo membership,
configuration and child working directory. They do not change the process cwd.
Root packages, custom member directories and dependency aliases retain their
Cargo identity; explicit lint paths keep their requested scope and exclusions.
`[workspace-scan].default_roots` can select a project's existing default Rust
source domain; an empty list follows Cargo targets and their declared modules.
Explicit scopes override those defaults. Configured dependency-direction checks
resolve the full graph through the selected context; ordinary contexts can load
empty workspaces. Prepare dependencies before an offline graph check.

Generic checks are enabled independently of Kithara ownership. Arc/Tokio,
cancellation, smoothing, Firewheel and Kithara-specific derive checks require
explicit subjects or enable/owner settings in the existing arch/idioms config.
Kithara's own configuration selects that policy; a consumer does not inherit
it from the dependency's checkout. README checks resolve declared or inherited
package metadata only for selected files, using each package's declared README
path. Unit tests use local policy fixtures; workspace tests own Kithara's actual
configuration and source contracts.

Clippy uses the configured Cargo tool role. Its optional
`[clippy].macos_features` defaults to an empty list; Kithara explicitly selects
its Apple product feature in its own config. Test lanes and health stages
continue to use the selected project's existing command/tool configuration.
Prepare dependency and executable inputs before an offline run; a prepared
locked consumer needs no adjacent Kithara checkout.

Everything project-specific comes from `.config/xtask.toml`, parsed once into
the context. The file is optional: a project with none gets documented code
defaults. A project puts its own sections under `[ext.*]`, which the core passes
through untouched, and registers only its own heavyweight commands as deep
stages. External programs are addressed by role under `[tools.<role>]`, so a
project that installs them elsewhere sets a path instead of patching the core.
Quality Lab intentionally keeps a separate, **required**
`.config/quality-lab.toml` so heavyweight analyzers never reach the fast lint
path. The ownership rules, the schema's strictness, and what each section is
allowed to declare are in [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-devtools).

### AST configuration

AST rule definitions belong to the caller's ordinary `sgconfig.yml` and its
native `ruleDirs` / `utilDirs`. This crate supplies execution and diagnostic
handling; it contains no embedded rule collection. Kithara keeps every maintained
rule source in root `.config/ast-grep`.

The default source scope follows Cargo targets and their modules; a standalone
`Ctx::new` scans its root without Cargo metadata. Explicit paths override that
scope. `[ast-grep].default_roots` owns the caller's AST domain independently of
Rust-only scan roots. `[lint_exclude]` owns test/tooling exclusions, while
`scan_all_rules` resolves identities and report categories from the same native
configuration. Unknown or duplicate identities fail explicitly. Strict mode
promotes warning diagnostics; execution errors and existing fix safeguards stay
in force.

An external consumer prepares any selected canonical rule exports explicitly
and supplies its own ordinary scan configuration. Generated exports retain the
source revision and content provenance; they are build artifacts, not another
maintained rule library. Exchange results with the exact source revision,
selected IDs, executable pin and consumer scope.

## Key Types

`CoreCommand` groups the shared commands into a few families:

- **Hygiene** — formatting, typo and ast-grep wrappers, Cargo manifest checks,
  per-package orphan detection, and the ratcheted `arch` / `style` / `idioms`
  lint namespaces. *(feature `lint`)*
- **Analysis** — recursive Rust type-shape and behavior similarity, parallel
  execution chains behind a fork, workspace health, public-surface comparison, feature-powerset checking, and the
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
- `viz` (default) - architecture visualization.

Architecture runtime scenarios use `kithara-devtools-trace-format` for versioned records and
JSONL writing. Importing, correlation, source matching, and graph rendering remain
in devtools.

`--no-default-features` drops those command families for a project that only
wants format, test, health, and friends.
