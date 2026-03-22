---
name: pr
description: Draft project-aligned PR descriptions from the current branch. Use when the user asks to prepare a PR description, PR notes, a branch summary for review, or to fill the repository pull request template. Follow `.docs/workflow/pr-description.md` for the output shape and expectations.
---

# PR Description

## When to use

- Use this skill for requests such as "prepare a PR description", "write PR notes", "summarize this branch for review", or "fill the PR template".

## Workflow

1. Determine the base branch for the current branch comparison.
2. Review the branch history from the merge-base to `HEAD`.
3. Review the current diff for the same range.
4. If there are no committed changes in that range, inspect the current staged and unstaged changes instead and draft the PR from that evidence.
5. Otherwise, include staged and unstaged changes only when the user explicitly asks for them or clearly wants them included in the draft.
6. If you see staged or unstaged changes while preparing the draft, explicitly offer the user to commit them before finalizing the PR description.
7. Derive every statement from actual commits, diffs, and validation evidence.

## Output rules

- Keep the description concise and reviewer-oriented.
- Use the repository source structure and section names.
- In `Summary`, explain the motivation and meaning of the change in plain reviewer-friendly language as prose, not as a checklist of bullets.
- Do not invent motivation, risks, validation steps, or follow-ups; derive them from commits, diffs, and validation evidence.
- If a required impact field has no evidence, write `none`.
- If validation was not run, say so explicitly.

## Repo note

- In this repository, use `.docs/workflow/pr-description.md` as the source of truth for the PR description shape.
- Do not invent a parallel template when the workflow document already defines the structure.
