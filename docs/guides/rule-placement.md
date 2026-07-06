# Agent Rule Placement

Keep mandatory context small. When a review finding repeats, put the rule at the
lowest layer that prevents the mistake.

- `AGENTS.md`: repo-wide blockers that should stop a design before code.
- `docs/workflows/rust-ai.md`: task flow, split, handoff, and reference routing.
- `docs/guides/*`: expanded red flags, examples, review guidance, and policy
  details loaded only when relevant.
- Owning crate `CONTEXT.md`: lifecycle, invariants, state ownership, protocol, or
  cache contracts for that crate.
- `ast-grep` / `xtask`: stable mechanical shapes caught as the last line of
  defense.
- Tool hooks or command guards: repeatable tool misuse, wrong test command,
  unsafe generated-output edit, or deterministic command mistakes.

Do not promote a rule upward just because it is true. Promote it only when agents
must see it earlier to avoid wasting review or validation time.
