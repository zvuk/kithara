# Open-Source Readiness — Practical Minimum (Variant A)

Date: 2026-02-13

## Goal

Prepare kithara for public release on a corporate GitHub account. One maintainer, open to community contributions.

## Scope

### 1. AGENTS.md — translate to English
- Full translation preserving structure and all rules
- Concise, imperative style (like codex AGENTS.md)
- CLAUDE.md stays as-is (already English, Claude Code-specific)

### 2. CONTRIBUTING.md — full rewrite
New structure:
1. Getting Started (fork, clone, build, test)
2. Development Workflow (branch naming, commits, TDD)
3. Pull Request Process (checklist, review)
4. Architecture Overview (link to crate graph)
5. First-Time Contributors (good first issues)
6. DCO (Signed-off-by for corporate open-source)
7. Code of Conduct link

### 3. CI — strengthen
- Add macOS test job (Apple AudioToolbox backend)
- Add MSRV check (1.85, edition 2024 minimum)
- Add cargo-deny job (deny.toml exists, not in CI)
- Add dependabot.yml (cargo weekly, github-actions monthly)

### 4. README — enhance
- Add Features section (bullet list)
- Add MSRV badge/note
- Clean up badge ordering

### 5. CODEOWNERS + small fixes
- Add .github/CODEOWNERS
- Remove /Cargo.lock from .gitignore (already tracked)
- Add rust-version = "1.85" to [workspace.package]

## Out of scope
- SPDX copyright headers
- Typos/spellcheck in CI
- YAML issue template forms
- Separate ARCHITECTURE.md
- Symphonia git dep resolution (upstream dependency)

## Reference projects
- [realm](https://github.com/spellshift/realm) — AGENTS.md, CLAUDE.md, claude.yml, PR template
- [trustee](https://github.com/confidential-containers/trustee) — CODEOWNERS, dependabot, cargo-deny
- [codex](https://github.com/openai/codex) — AGENTS.md, 6 issue templates, dependabot 6 ecosystems
- [xilem](https://github.com/linebender/xilem) — MSRV check, multi-OS CI, cargo-deny, typos
