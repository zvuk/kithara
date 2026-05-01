//! Apply a set of byte-range edits to a source string, validating that
//! they do not overlap.
//!
//! Used by autofix passes that compute new content for a handful of
//! disjoint regions (e.g. swapping struct-field block ranges) and need a
//! single string back.

#![allow(
    dead_code,
    reason = "engine lands ahead of its consumer; Phase 2 style-fix checks will call SourceRewriter (see common/fix/README.md)"
)]

use std::{fmt, ops::Range};

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum RewriteError {
    Overlap { a: Range<usize>, b: Range<usize> },
    OutOfRange { range: Range<usize>, src_len: usize },
    InvertedRange { range: Range<usize> },
}

impl fmt::Display for RewriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overlap { a, b } => write!(
                f,
                "edits overlap: {}..{} and {}..{}",
                a.start, a.end, b.start, b.end
            ),
            Self::OutOfRange { range, src_len } => write!(
                f,
                "edit range {}..{} exceeds source length {src_len}",
                range.start, range.end
            ),
            Self::InvertedRange { range } => write!(
                f,
                "edit range {}..{} is inverted (start > end)",
                range.start, range.end
            ),
        }
    }
}

impl std::error::Error for RewriteError {}

/// Stages a sequence of replacements over `src` and produces the rewritten
/// string in `finish()`. No write is performed until `finish()` is called.
pub(crate) struct SourceRewriter<'src> {
    src: &'src str,
    edits: Vec<(Range<usize>, String)>,
}

impl<'src> SourceRewriter<'src> {
    pub(crate) fn new(src: &'src str) -> Self {
        Self {
            src,
            edits: Vec::new(),
        }
    }

    /// Stage a replacement of `range` with `with`. Order of `replace` calls
    /// does not matter; `finish()` sorts and validates them.
    pub(crate) fn replace(&mut self, range: Range<usize>, with: impl Into<String>) {
        self.edits.push((range, with.into()));
    }

    /// True iff no replacements were staged. Caller can use this to skip
    /// `fs::write` and uphold invariant I3 (no-op safety).
    pub(crate) fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// Validate ranges, sort them, and apply replacements producing the new
    /// source. Errors out on overlap or out-of-range edits.
    pub(crate) fn finish(mut self) -> Result<String, RewriteError> {
        let src_len = self.src.len();
        for (range, _) in &self.edits {
            if range.start > range.end {
                return Err(RewriteError::InvertedRange {
                    range: range.clone(),
                });
            }
            if range.end > src_len {
                return Err(RewriteError::OutOfRange {
                    range: range.clone(),
                    src_len,
                });
            }
        }
        self.edits.sort_by_key(|(r, _)| r.start);
        for pair in self.edits.windows(2) {
            let a = &pair[0].0;
            let b = &pair[1].0;
            if a.end > b.start {
                return Err(RewriteError::Overlap {
                    a: a.clone(),
                    b: b.clone(),
                });
            }
        }
        let mut out = String::with_capacity(src_len);
        let mut cursor = 0_usize;
        for (range, with) in &self.edits {
            out.push_str(&self.src[cursor..range.start]);
            out.push_str(with);
            cursor = range.end;
        }
        out.push_str(&self.src[cursor..]);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_edits_returns_input_verbatim() {
        let r = SourceRewriter::new("hello world");
        assert!(r.is_empty());
        assert_eq!(r.finish().unwrap(), "hello world");
    }

    #[test]
    fn single_edit_replaces_inner_slice() {
        let mut r = SourceRewriter::new("hello world");
        r.replace(6..11, "claude");
        assert!(!r.is_empty());
        assert_eq!(r.finish().unwrap(), "hello claude");
    }

    #[test]
    fn multiple_disjoint_edits_apply_in_source_order() {
        // Insert order is reversed on purpose to verify sorting.
        let mut r = SourceRewriter::new("a b c d");
        r.replace(4..5, "X");
        r.replace(0..1, "Y");
        assert_eq!(r.finish().unwrap(), "Y b X d");
    }

    #[test]
    fn empty_range_is_an_insertion() {
        let mut r = SourceRewriter::new("ac");
        r.replace(1..1, "b");
        assert_eq!(r.finish().unwrap(), "abc");
    }

    #[test]
    fn replacement_can_grow_or_shrink() {
        let mut r = SourceRewriter::new("xxxx");
        r.replace(1..3, "");
        assert_eq!(r.finish().unwrap(), "xx");

        let mut r = SourceRewriter::new("xx");
        r.replace(1..1, "yyyy");
        assert_eq!(r.finish().unwrap(), "xyyyyx");
    }

    #[test]
    fn overlapping_edits_are_rejected() {
        let mut r = SourceRewriter::new("0123456789");
        r.replace(2..6, "X");
        r.replace(4..8, "Y");
        let err = r.finish().unwrap_err();
        assert!(matches!(err, RewriteError::Overlap { .. }), "got {err:?}");
    }

    #[test]
    fn out_of_range_edit_is_rejected() {
        let mut r = SourceRewriter::new("hi");
        r.replace(0..5, "x");
        assert!(matches!(
            r.finish().unwrap_err(),
            RewriteError::OutOfRange { .. }
        ));
    }

    #[test]
    fn inverted_range_is_rejected() {
        let mut r = SourceRewriter::new("abc");
        r.replace(2..1, "x");
        assert!(matches!(
            r.finish().unwrap_err(),
            RewriteError::InvertedRange { .. }
        ));
    }

    #[test]
    fn adjacent_edits_at_same_byte_are_allowed() {
        // Two edits ending and starting at the same byte do not overlap.
        let mut r = SourceRewriter::new("abcdef");
        r.replace(0..3, "X");
        r.replace(3..6, "Y");
        assert_eq!(r.finish().unwrap(), "XY");
    }

    #[test]
    fn idempotency_via_no_op() {
        // Running `finish()` on no edits, then feeding the result through a
        // fresh rewriter with no edits, gives the same output. Mirrors the
        // I2 invariant for the autofix engine.
        let r = SourceRewriter::new("stable").finish().unwrap();
        let r2 = SourceRewriter::new(&r).finish().unwrap();
        assert_eq!(r, r2);
    }
}
