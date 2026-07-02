# Review And Validation

Use this before handoff, PR notes, or after a substantial agent-written chunk.

## Review Discipline

- Review the diff as a reviewer would. Every hunk needs a one-sentence reason.
- Fix the whole bug class in the same concern: sibling entry points, sync/async
  twins, fast/slow paths, platform branches, and copied patterns.
- One concern per PR. Same-class fixes are one concern; drive-by refactors are
  not.
- Treat test expectation changes as suspicious until the contract change is
  explicit.
- Do not weaken, skip, or delete existing tests or safety nets silently.
- Comments must be load-bearing and true. If a comment explains a non-obvious
  invariant, make sure the code enforces it.

## Tests

- Every behavior change needs an automated test unless the task explicitly says
  otherwise.
- A useful regression test fails for the right reason on the old code.
- Every assertion must be able to fail and should assert the strongest useful
  invariant.
- Prefer exact normalized values over broad `contains` or "does not panic" tests.
- Tests must be deterministic, hermetic, and avoid external network.
- Do not use sleeps or timeouts to make races pass; await the condition or add a
  deterministic probe.

## Validation Reporting

- Report exact commands run and what they proved.
- Keep fast compile, focused tests, architecture lint, and broad test runs as
  separate signals.
- If validation is blocked or not run, say so explicitly.
- If a lint fails, open only the reported rule/check and fix the design cause
  rather than suppressing the finding.
