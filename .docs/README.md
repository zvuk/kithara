# Documentation Layout

`AGENTS.md` is the always-on repo contract. This directory owns the optional
documents and reusable agent assets that should be loaded only when relevant.

- `guides/`: lazy-loaded architecture, Rust style, review, tooling, and policy references.
- `workflows/`: task, planning, handoff, and PR-description workflows.
- `rules/`: tool-neutral entry rules exposed through tool-specific symlinks.
- `agents/`: concrete subagent/scenario definitions exposed through symlinks.
- `skills/`: reusable local agent skills.
- `plans/`: dated implementation plans and archived planning material.

Tool-specific folders such as `.claude/*`, `.cursor/*`, and top-level
`CLAUDE.md` / `GEMINI.md` / `WARP.md` are adapters. Keep canonical content here
unless the tool requires a compatibility path.
