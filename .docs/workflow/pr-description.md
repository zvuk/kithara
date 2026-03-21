# PR Description Contract

Use this file as the canonical shape for PR descriptions.

A good PR description should let the reviewer understand the change without
reopening the issue, agent handoff, or chat history.

Include:

- `Summary`: what changed and why;
- `Behavior / scope`: the main contract change and affected area;
- `Validation`: exact commands run locally, or an explicit note that validation was not run;
- `Surface impact`: whether public API, platform/runtime surfaces, or perf-sensitive paths changed;
- `Risks / follow-ups`: anything intentionally deferred, uncertain, or worth extra review attention.

Do not leave impact fields implicit. Write `none` when there is no impact.

Use this shape:

```md
## Summary
- what changed
- why it changed

## Behavior / scope
- contract or behavior: ...
- affected area: ...

## Validation
- `...`
- not run: ...

## Surface impact
- Public API: none | changed: ...
- Platform/runtime: none | changed: browser/wasm | apple | android | native app | tooling surface
- Perf-sensitive paths: none | changed: ...

## Risks / follow-ups
- none
```
