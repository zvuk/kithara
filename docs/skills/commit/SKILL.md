---
name: commit
description: Lint, stage, and commit changes following project conventions. Use when the user asks to commit, save changes, or says "/commit". Analyzes changes, runs linters, groups unrelated changes into separate commits, and writes conventional commit messages.
---

# Commit

## When to use

- The user asks to commit changes, make a commit, save progress, or invokes `/commit`.

## Commit message conventions

This project uses **Conventional Commits** with optional scope:

```
type(scope): short summary in lowercase
```

### Types

| Type | When |
|---|---|
| `feat` | new feature or capability |
| `fix` | bug fix |
| `refactor` | restructuring without behavior change |
| `docs` | documentation only |
| `test` | test-only changes |
| `chore` | tooling, CI, dependencies, config |

### Scope

- Scope matches the affected crate or area: `hls`, `audio`, `test-utils`, `ci`, `apple`, `play`, `stream`, etc.
- Multiple scopes are comma-separated: `feat(hls,audio): ...`
- Omit scope when the change spans many areas or is purely top-level.

### Message body

- Subject line: imperative mood, lowercase after prefix, no period, under 72 characters.
- If the diff is non-trivial, add a blank line and a short body listing what changed and why.
- Use a HEREDOC to pass the message to `git commit -m` for correct formatting.

## Workflow

1. **Inspect changes.** Run `git status` (never `-uall`) and `git diff` (staged + unstaged) to understand what changed.
2. **Group by intent.** If changes serve different purposes (e.g., a feature + an unrelated lint fix), propose splitting into multiple commits. Ask the user to confirm before proceeding.
3. **Run linters on changed files.**
   - `cargo fmt -p <crate> -- --check` for each affected crate.
   - If formatting fails, run `cargo fmt -p <crate>` to fix, then re-check.
   - `cargo clippy -p <crate> -- -D warnings` for each affected crate.
   - If clippy fails on a **pre-existing** issue unrelated to the current changes, note it but do not block the commit.
   - If clippy fails on **new code**, fix the issue before committing.
4. **Stage files.** Add specific files by name. Never use `git add -A` or `git add .`.
5. **Commit.** Write the message following the conventions above. Use a HEREDOC:
   ```sh
   git commit -m "$(cat <<'EOF'
   type(scope): subject line

   Optional body.
   EOF
   )"
   ```
6. **Verify.** Run `git status` after the commit to confirm a clean state or remaining changes.
7. If the pre-commit hook fails, fix the issue and create a **new** commit (do not amend).

## Rules

- Never amend unless the user explicitly asks.
- Never push unless the user explicitly asks.
- Never skip hooks (`--no-verify`).
- Never commit files that likely contain secrets (`.env`, credentials, keys).
- Do not stage unrelated changes into the same commit without user approval.
