# Progress

What is in flight right now. The
[GitHub Projects board](https://github.com/users/gerasim13/projects/3) owns
capability status and the roadmap, and git owns the facts. This file owns
intent: what is being worked on, what comes next, what is stuck. Update it in
the change that lands the work, and keep it short.

## In Flight

- Configuration document for `kithara-app`. `app.yaml` plus an optional overlay,
  merged and env-expanded before typing; every section carries the owning crate's
  own patch type, so a value is spelled once. `#[derive(Patch)]` in the
  new `kithara-macros` generates every patch and its `apply`; `struct-patch` and
  every hand-written patch struct are gone. The output rate is named once,
  under `app`. Secrets stay `$KITHARA_...` references and one resolving nowhere
  stops startup. Merged with `production/main`: `broadcast`, `player.warp`, and
  the stretch backends' preparation geometry under `player.warp.backends` are
  sections now, and `app.crossfade_seconds` is gone in favour of
  `player.crossfade_duration`. The two configs that grew thread budgets since
  carry the derive too: `play_worker` names the one playback worker's budgets and
  `dispatcher` names every app-owned dispatcher's, minus the thread name each
  construction site keeps.
- Harness and document revision. `AGENTS.md` routes instead of restating, and the
  `style` namespace budgets documents with `doc_size`, blocks drift with
  `doc_staleness`, and holds every README to `readme_shape`.

## Next

- Wire the last section end to end: the three integration harnesses still build
  pools from `PoolsSection::default()`, not from the document they load.
- Work the comment queue down by hand. `--fix` is exhausted - a second run on a
  clean tree changes nothing - so all 668 are decisions: 497 prose comments outside
  a doc comment, 105 long doc blocks, 50 oversized inline comments, 16 dense
  functions. A body comment has no mechanical home.
- 439 ordering findings are still mechanical: `struct_field_order` 160,
  `trait_item_order` 188, `struct_init_order` 91. One `just lint style --fix`
  clears them, but it rewrites declarations across every crate, so it wants its
  own change.
- Wire `just lint style` to a gate. Nothing runs it today - not the commit hook,
  not a CI lane - so the ratchet drifted unseen. A warm run is 58 s: too much per
  commit, nothing for a lane, and the lane catalog owns it.

## Blocked

- Nothing.
