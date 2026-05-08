//! Comment-preserving autofix engine for xtask checks.
//!
//! See `README.md` next to this module for the full contract: how block
//! ranges are computed, what counts as leading vs trailing trivia, and
//! the four invariants (I1-I4) that every check fix must satisfy.
//!
//! This module deliberately stays low-level: it does not know about specific
//! checks or about `Violation`. Each style/arch check that opts in will use
//! `BlockExtractor` to derive byte ranges and `SourceRewriter` to apply
//! non-overlapping edits.

pub(crate) mod block;
pub(crate) mod rewriter;

#[cfg(test)]
mod tests;

pub(crate) use block::{ExpansionError, expand_blocks};
pub(crate) use rewriter::SourceRewriter;

/// Outcome of running a single check's `fix()` across the workspace.
///
/// `writes` is the number of files modified. `skipped` lists every scope
/// the fix refused to touch with a human-readable reason — typically a
/// floating comment that would be torn from its item by reordering, a
/// macro-bound item with unreliable spans, or `#[cfg]` adjacency that
/// could change semantics. A single fix run can both patch some files
/// and skip others; both pieces of information are surfaced.
#[derive(Debug, Default)]
pub(crate) struct FixOutcome {
    pub(crate) writes: usize,
    pub(crate) skipped: Vec<String>,
}
