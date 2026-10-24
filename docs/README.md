# Documentation Layout

`AGENTS.md` is the always-on repo contract. This directory owns the optional
documents and reusable agent assets that should be loaded only when relevant.

- `guides/`: lazy-loaded architecture, Rust style, review, tooling, and policy
  references. Open the one that matches the trigger below, and nothing else.
- `workflows/`: task, planning, handoff, and PR-description workflows.
- `rules/`: tool-neutral entry rules exposed through tool-specific symlinks.
- `agents/`: concrete subagent/scenario definitions exposed through symlinks.
- `skills/`: reusable local agent skills.
- `plans/`: dated implementation plans and archived planning material.
- `specs/`: local-by-default specs (gitignored like plans, with only the
  directory marker tracked).
- `superpowers/`: symlinks that expose plans/specs to the superpowers tooling.

## Guide Index

| Guide | Open it when |
| --- | --- |
| `guides/rust-shape.md` | Rust idiom, naming, imports, visibility, file shape, or error quality |
| `guides/architecture-shape.md` | owner graphs, shared state, channels, god objects, callback flows, coupling |
| `guides/red-flags.md` | non-trivial work, a design check, a handoff, or a lint failure |
| `guides/rule-placement.md` | deciding where a new rule belongs |
| `guides/test-harness.md` | adding or debugging tests, a flaky test or a stress report, changing test utilities, explaining validation scope |
| `guides/tooling.md` | repo tooling, an autofix, formatter/lint config, dependency policy, `arch viz` or `quality assess` flags |
| `guides/lint-policy.md` | lint policy, a lint exception, or a lint you cannot resolve locally |
| `guides/performance.md` | a hot path, allocations, or a performance regression |
| `guides/cancel-policy.md` | touching cancellation |
| `guides/agent-hooks.md` | tool adapters, hook behavior, or command routing |
| `guides/review-validation.md` | before a handoff or PR notes, or after a substantial agent-written chunk |
| `guides/ci-host.md` | the dedicated CI host and its `xtask ci` ownership |
| `guides/ci-bridge.md` | repository synchronization, PR check projection, bridge deployment or the CI verdict |

Tool-specific folders such as `.claude/*`, `.cursor/*`, and top-level
`CLAUDE.md` / `GEMINI.md` / `WARP.md` are adapters. Keep canonical content here
unless the tool requires a compatibility path.
