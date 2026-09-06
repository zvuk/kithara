# Progress

What is in flight right now. The
[GitHub Projects board](https://github.com/users/gerasim13/projects/3) owns
capability status and the roadmap, and git owns the facts. This file owns
intent: what is being worked on, what comes next, what is stuck. Update it in
the change that lands the work, and keep it short.

## In Flight

- One owner of track analysis in `kithara-app`, `AnalysisService`, and one
  extent per pass in `kithara-analysis`. The grid is published at the tempo
  level the detector reports, tagged `grid_bpm_from_beats_v4`. Left: the
  reported deck scenario on a release build with the full model, and the size
  of the resume blob.

- `SpectralBeats`, a beat detector needing no model, beside the neural one. It
  searches the `Tempo` its caller hands it, and a build picks the model it
  embeds; the cache tag names both. Left: nothing.

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
