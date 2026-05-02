# `xtask::common::fix` — comment-preserving autofix engine

This module is the building block that future autofix passes (style-fix
checks in Phase 2 and beyond) sit on top of. It does not know about
specific checks or `Violation`s — its only job is to extract byte ranges
that own their associated comments, and apply non-overlapping byte
edits to the source string.

The contract here is what makes autofix safe to use on a codebase that
has hand-written explanatory comments scattered between items: nothing
is ever silently lost or moved away from the item it documents.

## Why a byte-level engine?

`syn::parse_file` discards every `//` line comment and every `/* */`
block comment that is not a doc-comment. Doc-comments (`///`, `//!`)
survive only because they are surfaced as `#[doc = "..."]` attributes.
A naive `syn` → `prettyplease` round-trip therefore deletes a large
fraction of the explanatory text in this codebase.

The fix is to treat each item's source text as an opaque blob: derive
its byte range from `proc_macro2::Span::byte_range()`, expand that range
to absorb attached comments, and then perform the rewrite at the byte
level. The AST is only used to *locate* items, never to re-emit them.

## The model: "block range"

Each item has a **block range** = byte slice from the start of its
*leading trivia* to the end of its *trailing trivia*.

### Leading trivia (absorbed into the block)

- Doc-comments (`///`, `//!`, `#[doc = "..."]`) — already part of the
  item span via `syn`. The engine refuses to absorb them from outside
  the span: that would either duplicate them or hide a bug in the
  caller's span construction.
- Outer attributes (`#[derive]`, `#[cfg]`, `#[expect]`, …) — already in
  the item span.
- Outer line comments (`// …`) and outer block comments (`/* … */`)
  immediately preceding the item, **only if there is no blank line
  between them and the item**. A blank line is an explicit boundary.

### Trailing trivia (absorbed into the block)

- Trailing comma `,` (for fields and init fields).
- Inline line comment (`// …`) on the same line as the item end.
- Inline block comment (`/* … */`) that opens *and closes* on the same
  line as the item end.

### Excluded from the block

- Leading blank lines — they remain as separators.
- The newline after the item — also remains as a separator.
- Block comments that span multiple lines after the item — they
  semantically belong to whatever follows, not to this item.

## Floating comments

A "floating" comment is one that is separated from every neighbouring
item by a blank line on at least one side, so neither neighbour's
expansion picks it up. The engine refuses to compute block ranges in
that case and returns `ExpansionError::FloatingComment { line, snippet }`.

The caller (a check's `fix()` implementation) must propagate this as a
**skip with reason**: the violation stays in the report, the file is
not modified, and the user is told which file:line contains the
floating comment so they can decide manually whether to delete it,
attach it to a neighbour by removing the blank line, or restructure.

This is intentional. Reordering a scope that contains a floating
comment would silently break its visual association with whichever
items are nearby — exactly the failure mode this whole engine exists
to prevent.

## Escape hatches (skip without writing)

A check's `fix()` should refuse to touch a scope (returning
`FixOutcome::Skipped { reasons }`) when:

- An item is inside a `macro_rules!` body or a macro invocation —
  spans there are unreliable.
- Adjacent items carry different `#[cfg(...)]` attributes —
  reordering can change semantics under different configurations.
- An item carries a custom proc-macro attribute that may rewrite its
  body.

These are caller-side decisions; the engine itself does not detect
them. Each check that uses `expand_blocks` is responsible for filtering
its candidate item list before passing it in.

## Invariants (golden-tested)

Every fix that uses this engine must uphold these four invariants. The
engine guarantees them by construction; the smoke tests in `tests.rs`
demonstrate that on a representative struct.

- **I1 — comment multiset preserved.** The set of comment texts (with
  multiplicity, ignoring position) before and after `fix()` is equal.
- **I2 — idempotency.** Running `fix()` a second time produces no
  further change.
- **I3 — no-op safety.** When there is nothing to fix, no file is
  written. `SourceRewriter::is_empty()` lets callers cheaply skip
  the write.
- **I4 — format-stable.** The engine never reorders or reformats bytes
  *inside* a block; blocks are moved around as opaque blobs.

## API surface

```rust
// block.rs
pub(crate) struct BlockRange {
    pub(crate) bytes: Range<usize>,         // including leading/trailing trivia
    pub(crate) item_bytes: Range<usize>,    // bare item span (for diagnostics)
}

pub(crate) enum ExpansionError {
    FloatingComment { line: usize, snippet: String },
    SpanOutOfRange { range: Range<usize>, src_len: usize },
    ItemsOutOfOrder,
}

pub(crate) fn expand_blocks(
    src: &str,
    scope_bytes: Range<usize>,        // content of the container (between { and })
    item_spans: &[Range<usize>],      // bare item spans, in source order
) -> Result<Vec<BlockRange>, ExpansionError>;

// rewriter.rs
pub(crate) struct SourceRewriter<'src> { /* ... */ }

impl<'src> SourceRewriter<'src> {
    pub(crate) fn new(src: &'src str) -> Self;
    pub(crate) fn replace(&mut self, range: Range<usize>, with: impl Into<String>);
    pub(crate) fn is_empty(&self) -> bool;
    pub(crate) fn finish(self) -> Result<String, RewriteError>;
}

// mod.rs
pub(crate) enum FixOutcome {
    NoOp,
    Patched { writes: usize },
    Skipped { reasons: Vec<String> },
}
```

## Why `#![allow(dead_code)]` on the engine modules?

This engine landed ahead of its first consumer (the Phase 2 style
fixes — `struct_init_order`, `struct_field_order`, `trait_item_order`).
Once that PR lands, the `dead_code` suppression will be removed.

The `Cargo.toml` workspace lints policy normally rejects this kind of
suppression (see `feedback_no_lint_suppression.md`). The exception is
recorded here per the policy ("if you must suppress, document why in
the owning crate `README.md` or in a short comment on the same item").
