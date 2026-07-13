# Tooling Policy

Use this when touching repo tooling, formatter/lint config, or dependency audit
policy. Keep `AGENTS.md` short; put command details here.

## Fast Gate

- `just fmt-check`: Rust fmt, Cargo manifest dependency order (`kithara-*`
  first), formatted non-Cargo TOML, and sorted JSON/JSONC.
- `just clippy`: workspace Clippy with warnings denied.
- `just ast-grep`: structural policy rules from `.config/ast-grep/`.
- `cargo xtask lint arch`: fast architecture gate used by pre-commit.

These are suitable for local pre-commit feedback.

## Full Audit

- `just audit`: scoped Rust fmt, Clippy, ast-grep, xtask lint, typos,
  similarity, and scoped orphan-module checks. With no scope, the orphan stage
  is latency-capped; `just health` owns the full workspace orphan sweep.
- `just lint-full`: fast lint plus xtask self-tests and quality scans.
- `just health`: broad local health report; heavy or environment-sensitive
  stages may report SKIP.
- Health owns the canonical argv for its xtask-backed stages and validates each
  command shape in the `kithara-devtools` unit tests.

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

- `cargo xtask format` is the formatter harness. Use `--check` for gates and
  `--only rust|manifest|toml|json|markdown` for scoped work.
- `rustfmt.toml` owns `.rs` formatting.
- `.config/tomlfmt.toml` plus `cargo-sort` provide the mechanical `Cargo.toml`
  write pass. `cargo xtask manifest dependency-order` owns the gate: internal
  `kithara` / `kithara-*` dependencies stay above external crates, and each
  dependency group stays sorted by key.
- Do not use `cargo sort --check` in gates: it conflicts with the repo's
  internal-first dependency policy after the post-pass.
- `taplo` owns non-Cargo TOML formatting.
- `tidy-json` owns JSON/JSONC sorting and formatting.
- `mdfmt` owns Markdown formatting. It is an explicit recipe/advisory health
  signal until the historical Markdown tree is cleaned up.
- Do not add a second formatter for the same file class unless the owner is
  changed here and in `justfile`.
