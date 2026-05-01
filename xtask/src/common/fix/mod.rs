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

#![allow(
    dead_code,
    unused_imports,
    reason = "engine lands ahead of its consumer; Phase 2 style-fix checks will adopt FixOutcome and the re-exports (see common/fix/README.md)"
)]

pub(crate) mod block;
pub(crate) mod rewriter;

#[cfg(test)]
mod tests;

pub(crate) use block::{BlockRange, ExpansionError, expand_blocks};
pub(crate) use rewriter::{RewriteError, SourceRewriter};

/// Outcome of running a single check's `fix()` against the workspace.
///
/// `Patched` means at least one file was modified. `Skipped` means the fix
/// bailed for a documented reason (floating comment, macro/cfg adjacency,
/// proc-macro attribute) and the violation remains in the report.
#[derive(Debug)]
pub(crate) enum FixOutcome {
    NoOp,
    Patched { writes: usize },
    Skipped { reasons: Vec<String> },
}
