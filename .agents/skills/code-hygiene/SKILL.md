---
name: code-hygiene
description: "Validate code hygiene against kithara conventions: strict comment cleanup, ASCII/English-only sources, visibility, surface/protocol crate boundaries. Runs on the diff - aggressively flags AI-ish/redundant comments, block-label comments, status/handoff/commit markers, bloated doc comments. Triggers: clean up comments, check code hygiene, code hygiene, scrub AI code, validate comments, tidy before commit, review hygiene, check doc comments."
allowed-tools: Read Edit Bash Grep
---

# code-hygiene: kithara code hygiene validator

This skill reproduces the manual cleanup the repo owner does: it **aggressively**
strips noise comments and AI artifacts, keeps sources ASCII/English-only, and
narrows over-broad visibility. It works **on the diff** (changed lines), not on
whole files.

**Removal is the default for inline comments (`//` in the code body).** Such a
comment must *earn* its place: only one that carries a non-obvious **why** a
competent reader cannot infer from the code survives. When in doubt, flag for
removal.

**Public-API doc comments (`///`/`//!` on a `pub` item) are NOT deleted.** A short
`///` on a public type/function/field/variant is an API contract. **Compress them
to a few lines** rather than deleting. Remove doc entirely only on private/trivial
items and on tests.

Rules come from project conventions (`AGENTS.md`, `.docs/workflow/rust-ai.md`) and
the owning crates (`README.md`/`CONTEXT.md`). The skill **does not rewrite
silently**: mechanical findings are fixed after confirmation; judgment-call
findings are listed (out-of-scope edits go to chat, not applied silently).

## When to apply

- Before a commit, or before handing off a logically complete chunk of work.
- After an AI agent wrote/changed code under `crates/`.
- When asked to "clean up comments", "scrub the AI code", "check hygiene".

## Scope: what to check

1. Pick the comparison base:
   - uncommitted changes -> `git diff` (+ `git diff --cached`);
   - otherwise branch vs main -> `git diff $(git merge-base HEAD main)..HEAD`.
2. Take **only added/changed lines** (`+` in the diff) and their nearby context.
   Do not audit whole files - that is out of scope.
3. Ignore generated files (`Cargo.lock`, `kithara-workspace-hack`).

## Rules (by frequency in real cleanups)

### 1. Inline comment that explains the code ("what"/"how")
Concerns **inline `//` in the code body** (not a `///` doc on a public item - see
rule 3). Delete any such comment that describes *what* or *how* the code does.
Keep **only** a non-obvious *why*: a subtle gotcha, an explanation of non-trivial
business logic, an invariant, a `SAFETY` note, a workaround for a specific bug.

Flag aggressively (when in doubt, flag):
- line-by-line narration - `// i += 1` above `i += 1`, `// release the lock`
  above `lock.release()`;
- **block-label comment** - `// fill the decode buffer`, `// mock fetcher`,
  `// commit the fetched segments`: it only announces the next block. The meaning
  belongs in a function/variable name (extract the block into a named helper),
  not in a caption above it;
- in-function divider banners (`// --- setup ---`) - a sign the function should be
  split, not commented.

Counter-check before keeping: remove the comment - does the code become less clear
to a competent reader? If not (or if a rename fixes it), delete it.

### 2. Status / phase / handoff / history / commit reference
Delete process-marker comments. Detectors (grep over `+` lines):
```
(stage|phase)\s*[0-9]   |   \bTODO\b|\bFIXME\b|\bHACK\b|\bXXX\b
commit\s+[0-9a-f]{6,}   |   \b[0-9a-f]{7,40}\b (hashes in prose)
for now|temporarily|in the future|later on
```
A comment must not address a human/agent or record "what was done" - it is a
contract, not a journal.

### 3. Bloated doc comment
`///` and `//!` are a **1-3 line** contract. The goal is to **shrink to a sensible
size**, not delete. Public API always keeps a short doc (see the principle above);
private and trivial items may have none.

What to do with a long doc:
- **public item** (`pub fn`/`struct`/`enum`/`trait`/`const`/field/variant) ->
  **compress** to 1 (max 2) lines of purpose; do not delete to zero;
- **private/trivial** item -> compress or remove;
- `#[test]` function -> remove the doc entirely.

What to cut from the doc body (but not the public item's doc itself):
- motivation/backstory/narrative paragraphs, decision logs, forward-looking notes;
- expanded "how it works inside" - state the mechanism in one phrase;
- duplication of the function body; restating parameter types/names.

Beyond 3 lines, keep only a non-obvious invariant that cannot be stated shorter -
that is the owner's call, not the agent's. Long contracts/invariants live in the
owning crate's `CONTEXT.md`, not in a doc comment. The "sensible limit": one or two
crisp sentences on *what it is and why*, without unrolling the implementation.
```
- /// A fetched media segment.
- ///
- /// Pulled from the HLS variant playlist, cached on disk, then decoded... (5 lines of narrative)
+ /// A fetched media segment.

// public item: compress, but do NOT delete
- /// Byte offset of the segment within the variant stream.   <- keep, short
```

### 4. ASCII and English only in sources
Code, comments, doc, and **string literals** must be English, ASCII only. Cyrillic
and any non-ASCII characters are forbidden everywhere in sources. Fix by replacing
with the ASCII equivalent (for punctuation) or translating to English (for text):
- unicode arrows -> `->`, `<->`;
- em/en dash `--` -> `-`;
- typographic quotes (`<<`/`>>`, curly double/single) -> `"` / `'`;
- box-drawing and math symbols (diagrams, set symbols, etc.);
- Cyrillic in comments/doc/literals -> translate to English.

Literals are also ASCII/English only: `panic!`/`expect`/`unreachable!`/`assert!`
messages, proc-macro diagnostics `Error::new(span, "...")`, `bail!`/`anyhow!`.
A literal is a runtime/diagnostic value - keep it English.

Detector (catches any non-ASCII):
`grep -nP '[^\x00-\x7F]'` over changed lines.

### 5. ASCII-art diagrams in doc
State machines, graphs, and box-drawing inside `///`/`//!` - delete; the contract
is expressed in prose and the signature, not a picture.

### 6. Over-broad visibility
A `pub` item reachable only within its own crate -> `pub(crate)` (the default
visibility per `AGENTS.md`). Source of truth - the compiler lint:
```
RUSTFLAGS="-W unreachable_pub" cargo check -p <crate>
```
Each `unreachable_pub` warning is a `pub(crate)` candidate. Do not touch what is
exported outward or reached through a trait/ABI/FFI/`#[no_mangle]`. Public named
types crossing crate boundaries should be `#[non_exhaustive]`.

### 7. Surface/protocol specifics in shared crates
Surface specifics (Apple, Android, browser, FFI, demo UI) must not leak into shared
crates - they belong in the matching adapter/app layer (`kithara-app`,
`kithara-ffi`, and the platform adapters). Protocol specifics (HLS-, file-) must
not live in shared crates - they stay in the protocol crates (`kithara-hls`,
`kithara-file`). Shared crates own reusable stream, storage, decode, playback, and
runtime abstractions - not surface or protocol policy.

Shared media types (`AudioCodec`, `ContainerFormat`, `MediaInfo`) live in
`kithara-stream`. A duplicate of such a type in another crate is a violation - flag
it and name the canonical owner. (See `.docs/workflow/rust-ai.md` cross-domain
guardrails.)

### 8. Code -> docs reference direction
References between code and docs are one-way: doc -> code, never the reverse. Code
(including doc comments) does not reference doc files - the normative document
points at the code, the code is self-contained. Flag, in sources and doc comments,
references to a crate `README.md`/`CONTEXT.md`, the root `CONTEXT.md`, and
`.docs/*.md` (plans, workflow, notes).

### 9. Opaque abbreviations in names
Identifiers without puzzle abbreviations (`segment_index`, not `si`). Prefer
standard plain words (`open`, `new`, `read`, `write`, `seek`, `send`, `recv`).
Flag new short, opaque names.

## Workflow

1. Build the diff (scope above), extract `+` lines with `file:line`.
2. Run the detectors from rules 2/4/8 (mechanical grep) plus a judgment pass for
   rules 1/3/5/6/7/9. Judge comments (1/3/5) under the removal default.
3. Group findings by category; for each give `file:line` and a short "why".
4. Split into:
   - **Mechanical** (ASCII/English rule 4, "what"/label comment rule 1, status
     markers rule 2, ASCII-art rule 5) - propose an autofix; apply via Edit after
     a "yes".
   - **Judgment** (bloated doc rule 3, visibility rule 6, surface/protocol leak
     rule 7, names rule 9) - list them for a decision, **do not edit silently**.
5. After edits, check the build and lints. Do not silence or delete tests to go
   green; `#[allow(...)]`/`baseline.toml` lint suppressions are forbidden
   (`AGENTS.md`).

## Verification after edits

```
cargo build --workspace
just lint-fast          # fast policy checks (style, idioms, arch, ast-grep)
just test-all           # nextest + doctests
```
Run `just lint-full` (the full set) before handing off. Removing `use`/code can
leave dangling imports or break a still-used dependency in `Cargo.toml` - the
build/lints catch this.

## Report format

```
Code hygiene: <N> findings in <M> files (base: <ref>)

Mechanical (proposed fixes):
  ASCII (r4)         crates/kithara-stream/src/info.rs:117  arrow -> '->'
  "What" comment (1) crates/kithara-play/src/runtime.rs:42  duplicates the code
  Block label (1)    crates/kithara-hls/src/loader.rs:91    '// mock fetcher' -> into a name
  Status marker (2)  crates/kithara-app/src/boot.rs:8       '(stage 4c)'

For decision (untouched):
  Bloated doc (3)    crates/kithara-stream/src/info.rs:30-39  9 lines of narrative -> compress?
  Visibility (6)     crates/kithara-net/src/port.rs:72        unreachable_pub -> pub(crate)?
  Surface leak (7)   crates/kithara-decode/src/apple.rs:14    Apple specifics in a shared crate?
```
Fix mechanical findings only after confirmation; never run history-rewriting fixes.
