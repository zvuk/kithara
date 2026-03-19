# FSM Crate Selection Debate — Synthesis

**Date:** 2026-03-14
**Participants:** Codex (pro-statig), Gemini (pro-hand-rolled), Claude (moderator)
**Topic:** Should kithara adopt an FSM crate? If so, which one?

---

## Research Phase Results

### Crates evaluated (Gemini + Codex parallel research):

| Crate | Stars | Last Update | Async | Per-State Data | Compile-Time Safety | HSM | Verdict |
|-------|-------|-------------|-------|----------------|---------------------|-----|---------|
| **statig** | ~750 | Jul 2025 | Native | Best | Medium-High | Yes | **Top choice** |
| **tokio-fsm** | ~150 | Feb 2026 | Native | Medium | Highest | No | New, promising |
| **rust-fsm** | ~250 | Jul 2025 | None | Manual | Medium | No | OK for simple |
| **finny** | small | Jan 2022 | No | Yes | Partial | Yes | Stale |
| **sm** | stable | 2019 | No | No (ZST) | Yes | No | Too rigid |
| **typestate** | stable | 2021 | No | Yes | Yes | No | Too rigid |
| **sfsm** | ~10 | May 2022 | No | Yes | Partial | No | Stale/embedded |
| **machine** | small | old | No | Yes | No | No | Stale |

### Consensus from research: `statig` is the best fit if adopting a crate.

---

## Debate Summary

### Key Arguments FOR `statig` (Codex):

1. **Hierarchy is real**: `WaitingForSource` is already a de facto superstate with 4 sub-cases. Readiness logic is duplicated across step_seek_requested, step_applying_seek, step_waiting_for_source, step_recreating_decoder.
2. **Already building a framework**: The proposed V5 improvements (sealed traits, guard macros, logging) = homegrown framework without guarantees.
3. **DecoderSession fits statig's model**: Already lives outside TrackState (shared storage pattern).
4. **Blocking mode exists**: Sync OS thread is first-class in statig, no async tax.
5. **Transition hooks centralize concerns**: ServiceClass, notifications, tracing in one place.

### Key Arguments FOR Hand-Rolled (Gemini):

1. **Call stack transparency**: 1:1 mapping between code and state transition in debugger. No macro-generated dispatch.
2. **Ownership precision**: Deep nesting (WaitContext -> RecreateState -> RecreateNext -> SeekRequest) with zero friction. Move semantics are explicit.
3. **Timing control**: Audio needs precise side-effect ordering. Hooks run when the library says, not when you need.
4. **Institutional knowledge**: 4 iterations, 9 bugs fixed. Team understands every branch.
5. **Migration cost**: 400+ lines of FSM code, extensive tests. Real risk of regression.
6. **Zero dependency**: No upstream breakage, no version conflicts, no supply chain risk.

### Concessions Made:

- **Codex concedes**: Explicit match-driven transitions make side effects obvious. Hooks should be thin coordination points, not behavior hiding.
- **Gemini concedes**: `statig`'s shared storage + hierarchy is objectively cleaner for complex FSMs. Hand-rolled stops scaling at ~10 states or when inter-state complexity grows.

---

## Moderator Verdict

### Recommendation: HYBRID APPROACH (Selective Adoption)

The debate revealed that the answer is not binary. The project has FSMs of vastly different complexity:

#### Keep Hand-Rolled:
- **Player Track FSM** (6 states, Copy enum, simple transitions) — too simple for a crate
- **Storage Resource FSM** (3 states, implicit transitions) — trivial, no benefit from crate
- **Source Phase** (observable, not behavioral) — not a behavioral FSM at all
- **Asset Resource State** (read-only view) — derived, not a machine

#### Evaluate `statig` for:
- **Audio Track FSM** (8 states, nested context, guards, retry logic) — the ONLY FSM complex enough to justify a crate dependency

#### The pragmatic path:

1. **Phase 1 — Borrow concepts, not code**: Refactor hand-rolled Audio Track FSM using `statig`-inspired patterns:
   - Extract `TrackContext` (shared mutable data) from state enum
   - Implement explicit `on_enter`/`on_exit` methods for each state
   - Add superstate handler for `WaitingForSource` family
   - Add `#[must_use]` on state transition results

2. **Phase 2 — Prototype with `statig`**: Write a `statig`-based implementation of the Audio Track FSM in parallel (feature-gated). Compare:
   - Lines of code
   - Debuggability (step through both with debugger)
   - Test coverage parity
   - Compile time impact

3. **Phase 3 — Decide based on evidence**: If `statig` prototype is measurably better (fewer lines, same debuggability, no perf regression) — adopt. If not — keep Phase 1 improvements.

### Why NOT immediate full adoption:
- Migration risk in the most critical audio path
- No compile-time transition validation (the main theoretical benefit of FSM crates) — `statig` doesn't provide this
- The current code works and is well-tested
- The biggest wins come from design patterns, not from the crate itself

### Why NOT pure hand-rolled forever:
- The Audio Track FSM is already a framework (phase_tag, step_*, WaitContext nesting)
- V5 improvements (sealed traits, macros) = building a worse version of `statig`
- If the FSM grows beyond 10 states, hand-rolled becomes a liability
- Hierarchy is a real need, not a nice-to-have

---

## Action Items

1. Read `statig` documentation and examples in detail
2. Create a feature branch with `statig` prototype for Audio Track FSM
3. Benchmark compile time, runtime, and debuggability
4. Decision point: after prototype is complete and tested
