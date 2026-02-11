# Open-Source Readiness Evaluation

Assessment of the kithara project's readiness for public release.

**Date:** 2026-02-11
**Overall score:** 7/10 — technically mature, not yet ready for publication

Engineering quality is high and exceeds many public Rust projects.
The gaps are in the "wrapper" around the code — what a potential contributor
or user sees on first contact with the repository.

---

## What is done well

### Architecture — excellent

- 12 modular crates with clear responsibility boundaries.
- Correct dependency hierarchy, no cycles.
- Workspace-first approach — all dependency versions in root `Cargo.toml`.
- Patterns (`StreamType`, decorator for assets, event-driven) applied properly.

### Code quality — excellent

- Strongly typed errors via `thiserror` in every crate (no `anyhow`, no `String`).
- Zero `unwrap()`/`expect()` in production code (except justified FFI spots).
- Zero `println!`/`dbg!` — only `tracing`.
- One TODO in the entire project (Android stub).
- `unsafe` isolated in FFI modules (`apple.rs`); all other crates use `#[forbid(unsafe_code)]`.

### Linting and CI — excellent

- `clippy.toml`: `unwrap_used = "deny"`, allowed only in tests.
- `deny.toml`: license checks, advisories, duplicate detection.
- `rustfmt.toml`: strict, uniform style.
- 26+ clippy lints enabled at workspace level.
- CI pipeline: lint → test → coverage (codecov with 80%/70% thresholds).
- Custom `lint-style.sh` for additional checks.

### Tests — good

- ~74 test files, unit + integration.
- Test infrastructure: rstest, unimock, serial\_test, MockNet.
- tarpaulin + codecov integration.
- Performance tests in CI.

### Code documentation — good

- 11 of 12 crates have `README.md` with diagrams and code examples.
- All `lib.rs` files contain `//!` module doc comments.
- Public traits are documented.

### Examples — good

- 9 examples across 5 crates (rodio playback, ABR simulation, segment analysis).

---

## Critical gaps

### P0 — must fix before publication

#### 1. No LICENSE file

`Cargo.toml` declares `license = "MIT OR Apache-2.0"`, but there are **no physical
license files** in the repository. Without them:

- The code is legally unlicensed — it cannot be used.
- crates.io will reject publication.
- No serious user or company will adopt the project.

**Action:** create `LICENSE-MIT` and `LICENSE-APACHE` at the repository root.
This is the standard for dual-licensed Rust projects (tokio, serde, etc.).

#### 2. Incomplete crate metadata

None of the 12 crates contain in their `Cargo.toml`:

- `repository = "https://github.com/<owner>/kithara"`
- `homepage = "..."`
- `documentation = "..."`

Only 3 of 12 crates have a `description` field. The remaining 9 inherit from
workspace, but for crates.io each crate should have its own description.

**Action:** add `repository` and a crate-specific `description` to every
`Cargo.toml` under `crates/`.

#### 3. Git dependency on symphonia

```toml
symphonia = { git = "https://github.com/pdeljanov/Symphonia.git", branch = "dev-0.6" }
```

Git dependencies cannot be published to crates.io. Before publication either:

- Wait for the symphonia 0.6 release, or
- Fork and publish under a different name, or
- Vendor the dependency.

---

### P1 — strongly recommended

#### 4. Create CONTRIBUTING.md

`AGENTS.md` contains detailed development rules (212 lines), but it targets
AI agents, not humans. An external contributor does not know:

- How to run tests.
- Which git workflow to follow (rebase? squash?).
- How to format a PR.
- Which areas are open for contributions.

**Action:** create `CONTRIBUTING.md` based on `AGENTS.md` and `CLAUDE.md`,
written for human contributors. Include:

```
## Quick start
cargo build --workspace
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace -- -D warnings

## Pull request checklist
- [ ] `cargo fmt` passes
- [ ] `cargo clippy` passes with no warnings
- [ ] New code has tests
- [ ] No `unwrap()`/`expect()` in production code
```

#### 5. Create SECURITY.md

For a project that works with HTTP, HLS, and DRM/AES — the absence of a
security disclosure policy is a significant gap.

**Action:** create `SECURITY.md` with:

- Supported versions.
- How to report vulnerabilities (email, not public issue).
- Expected response time.

#### 6. Add badges to root README

The README has no status indicators.

**Action:** add to the top of `README.md`:

```markdown
[![CI](https://github.com/<owner>/kithara/actions/workflows/ci.yml/badge.svg)](...)
[![codecov](https://codecov.io/gh/<owner>/kithara/branch/main/graph/badge.svg)](...)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](...)
```

#### 7. Fix mixed language in README

Line 39 of `README.md` contains Russian text:

> `kithara-bufpool` -- cross-cutting: используется всеми крейтами для
> zero-allocation буферов.

**Action:** replace with English for international audience.

#### 8. Add License section to README

**Action:** add at the bottom:

```markdown
## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
```

#### 9. Create README for kithara-drm

`kithara-drm` is the only crate without a `README.md`.

**Action:** create `crates/kithara-drm/README.md` following the pattern of
other crate READMEs.

---

### P2 — nice to have

#### 10. CODE_OF_CONDUCT.md

Standard expectation for OSS projects. Without it GitHub will not show the
"community health" badge.

**Action:** adopt [Contributor Covenant](https://www.contributor-covenant.org/).

#### 11. CHANGELOG.md

All crates are at `0.1.0`. A changelog is expected at publication.

**Action:** adopt [Keep a Changelog](https://keepachangelog.com/) format.

#### 12. GitHub issue and PR templates

**Action:** create:

- `.github/ISSUE_TEMPLATE/bug_report.md`
- `.github/ISSUE_TEMPLATE/feature_request.md`
- `.github/PULL_REQUEST_TEMPLATE.md`

#### 13. Document how to run examples

The 9 examples exist but there is no top-level guide on running them.

**Action:** add an `## Examples` section to `README.md` or create a
dedicated `examples/README.md`.

#### 14. Verify docs.rs compatibility

**Action:** run `cargo doc --workspace --no-deps` in CI and confirm it
succeeds without warnings. Add a `[package.metadata.docs.rs]` section where
needed.

---

## Comparison with reference projects

| Aspect               | kithara       | tokio          | symphonia    |
| -------------------- | ------------- | -------------- | ------------ |
| LICENSE file         | missing       | MIT + Apache   | MPL-2.0      |
| CONTRIBUTING         | missing       | present        | present      |
| CI/CD                | present       | present        | present      |
| Code quality         | high          | high           | high         |
| Examples             | 9             | extensive      | present      |
| crates.io metadata   | incomplete    | complete       | complete     |
| Badges               | none          | present        | present      |
| Changelog            | missing       | present        | present      |

---

## Summary

The project is engineering-mature: architecture, code quality, testing, and
linting are at the level of the best Rust projects. The gaps are exclusively in
"packaging" — standard files expected in the open-source ecosystem. Closing the
P0 items is required before any publication; P1 items should follow shortly
after.
