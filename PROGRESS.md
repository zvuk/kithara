# Progress

What is in flight right now. The
[GitHub Projects board](https://github.com/users/gerasim13/projects/3) owns
capability status and the roadmap, and git owns the facts. This file owns
intent: what is being worked on, what comes next, what is stuck. Update it in
the change that lands the work, and keep it short.

## In Flight

- Tooling parameter ownership. Every policy number and list `xtask` and
  `kithara-devtools` used to carry as a `const` now has a config owner: eight
  keys of the CI host profile, the architecture thresholds, the style checks,
  and the stress, quality and architecture render budgets. Programs the tools
  spawn are resolved through the profile rather than spelled into the call, and
  the shell strings the two crates embedded are gone - one Rust binary per crate
  stands in for what a script did. Moving a `const` into a container that
  derives `Default` would have handed the reader zeroes: eleven structs got a
  written `Default` instead, each listing every field. The stand-in binary took
  `xtask` past what the `cargo xtask` alias can resolve on its own, so the
  manifest now names `default-run`.
- Harness and document revision. `AGENTS.md` routes instead of restating, and
  the `style` namespace now budgets documents with `doc_size`, blocks drift with
  `doc_staleness`, and holds every crate README to one shape with `readme_shape`:
  a header that stays inside the package, badges keyed to `publish` and to the
  manifest's license, a `# <package name>` title, then `Usage` / `Key Types` /
  `Features` / `Integration` and nothing else. All three queues are at zero, and
  the rewrites turned up claims the sources contradict - a wrong feature list, a
  file that no longer exists, an inverted description of a known leak, an MPL-2.0
  crate wearing the MIT badge, two crates naming a dead owner, and a logo no
  published crate page could load.

## Next

- `xtask/tests/` still writes ten `#!/bin/sh` fixtures - eight in
  `self_cache_runner.rs`, two in `agent_hook_runner.rs`. They stand in for the
  hook and the cache runner's own callers, which are shell by contract, so they
  are the one place a script is the subject rather than the implementation.
- Work the comment queue down by hand. `--fix` is exhausted for comments - a
  second run on a clean tree changes nothing - so all 668 are decisions: 497
  comments carrying prose outside a doc comment, 105 doc blocks past a dozen
  lines, 50 oversized inline comments, 16 dense functions. A body comment has no
  mechanical destination.
- 439 ordering findings are still mechanical: `struct_field_order` 160,
  `trait_item_order` 188, `struct_init_order` 91. One `just lint style --fix`
  clears them, but it rewrites declarations across every crate, so it wants its
  own change.
- Wire `just lint style` to a gate. Nothing runs it today - not the commit hook,
  not a CI lane - which is why the ratchet drifted unseen. A warm run is 58 s:
  too much for every commit, nothing for a lane. The lane catalog owns that
  change, so it does not belong in this one.

## Blocked

- Nothing.
