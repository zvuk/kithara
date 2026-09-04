# Progress

What is in flight right now. The
[GitHub Projects board](https://github.com/users/gerasim13/projects/3) owns
capability status and the roadmap, and git owns the facts. This file owns
intent: what is being worked on, what comes next, what is stuck. Update it in
the change that lands the work, and keep it short.

## In Flight

- One owner of track analysis in `kithara-app`: `AnalysisService` holds the
  runner, the cache, the persistence client, and one `watch` value per
  analysed resource; a deck observes the track it shows and mirrors every
  revision. Readiness is completeness alone. A pass has one extent, the
  source's claim bounded by what it proved, and publishes once more when the
  reading ends, ahead of the trailing detection; a resumed pass starts like
  a fresh one. Left: the reported deck scenario on the release build with the
  full model, the size of the resume blob, and the mp3 first packet that
  `ComposedDecoder` labels short of its PTS after the codec's internal trim,
  which leaves a hole no pass can fill
  (`a_track_shorter_than_its_header_claims_is_completed` pins it red).

- Harness and document revision. `AGENTS.md` routes instead of restating; the
  `style` namespace budgets documents with `doc_size`, blocks drift with
  `doc_staleness`, and holds every crate README to one shape with
  `readme_shape`. All three queues are at zero.

## Next

- Work the comment queue down by hand: `--fix` is exhausted for comments, so
  all 668 are decisions (497 body comments, 105 long doc blocks, 50 oversized
  inline comments, 16 dense functions).
- 439 ordering findings are mechanical; one `just lint style --fix` clears
  them but rewrites declarations across every crate, so it wants its own
  change.
- Wire `just lint style` to a gate: nothing runs it today. A warm run is 58 s,
  too much for every commit, nothing for a lane. The lane catalog owns that.

## Blocked

- Nothing.
