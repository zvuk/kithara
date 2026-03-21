# WARP.md

Use `AGENTS.md` as the repo entrypoint.

This repo does not keep a separate `.codex/rules/` layer. Keep Codex routing here and in the canonical repo docs.

Then load:

- `.docs/workflow/rust-ai.md`
- the matching crate `README.md` files
- `tests/README.md` when touching tests, fixtures, browser coverage, or repo tooling

This file stays intentionally thin so the repo has one process source of truth.
