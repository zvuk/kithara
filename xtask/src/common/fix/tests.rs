//! Engine-level smoke tests that exercise `expand_blocks` and
//! `SourceRewriter` together to validate the four invariants from the
//! contract (see `README.md`):
//!
//! - **I1** comment multiset preserved
//! - **I2** idempotent: re-running the fix produces no further change
//! - **I3** no-op safety: when nothing needs fixing, the source is left
//!   byte-identical
//! - **I4** format-stable: the engine itself never reorders bytes inside
//!   a block, so any pre-formatted block stays formatted
//!
//! Per-check golden tests live next to each Phase 2 style check; this
//! module proves the underlying engine is sound.

use std::collections::BTreeMap;

use super::{block::expand_blocks, rewriter::SourceRewriter};

/// Count every line/block comment occurrence in `src` as a `(text, count)`
/// multiset, ignoring position. Used for invariant I1.
fn comment_multiset(src: &str) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            let text = src[start..i].trim_end().to_string();
            *counts.entry(text).or_insert(0) += 1;
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            let text = src[start..i].to_string();
            *counts.entry(text).or_insert(0) += 1;
            continue;
        }
        i += 1;
    }
    counts
}

#[test]
fn comment_multiset_counts_line_and_block_comments() {
    let src = "// a\n/* b */\nfn x() { /* c */ }\n// a\n";
    let counts = comment_multiset(src);
    assert_eq!(counts.get("// a"), Some(&2));
    assert_eq!(counts.get("/* b */"), Some(&1));
    assert_eq!(counts.get("/* c */"), Some(&1));
}

/// Round-trip: expanding blocks then re-emitting them in original order
/// reproduces the source byte-for-byte (engine is non-destructive).
#[test]
fn engine_round_trip_is_identity_for_basic_struct() {
    let src = "    a: u32,\n    b: u32,\n    c: u32,\n";
    let spans = vec![4..10, 16..22, 28..34];
    let blocks = expand_blocks(src, 0..src.len(), &spans).unwrap();

    let mut rw = SourceRewriter::new(src);
    for b in &blocks {
        let text = src[b.bytes.clone()].to_string();
        rw.replace(b.bytes.clone(), text);
    }
    let out = rw.finish().unwrap();
    assert_eq!(out, src, "identity round-trip changed source");
}

/// Reverse three blocks and verify the comment multiset is preserved.
/// Demonstrates that `///` doc-comments stay glued to their item, that
/// outer line comments without a blank line follow their item, and that
/// trailing inline comments (`//` and `/* */`) move with the trailing
/// comma.
#[test]
fn engine_reverse_three_fields_preserves_comments() {
    let src = "\
    /// doc-a
    a: u32, // tail-a
    // outer-b
    b: u32,
    c: u32, /* tail-c */
";
    let span_a_start = src.find("/// doc-a").unwrap();
    let span_a = span_a_start..src[span_a_start..].find("u32").unwrap() + span_a_start + 3;
    let span_b_start = src.find("b: u32").unwrap();
    let span_b = span_b_start..span_b_start + 6;
    let span_c_start = src.find("c: u32").unwrap();
    let span_c = span_c_start..span_c_start + 6;

    let blocks = expand_blocks(src, 0..src.len(), &[span_a, span_b, span_c]).unwrap();
    assert_eq!(blocks.len(), 3);

    // Reverse positions: block[0] gets text of block[2], block[2] gets
    // text of block[0]. block[1] stays.
    let texts: Vec<String> = blocks
        .iter()
        .map(|b| src[b.bytes.clone()].to_string())
        .collect();
    let mut rw = SourceRewriter::new(src);
    rw.replace(blocks[0].bytes.clone(), texts[2].clone());
    rw.replace(blocks[2].bytes.clone(), texts[0].clone());
    let after = rw.finish().unwrap();

    // I1: comment multiset preserved.
    assert_eq!(
        comment_multiset(src),
        comment_multiset(&after),
        "I1 violated: comment multiset changed\nbefore: {src}\nafter: {after}"
    );
    // Every distinguishing comment still present.
    assert!(after.contains("// tail-a"), "tail-a comment lost: {after}");
    assert!(
        after.contains("// outer-b"),
        "outer-b comment lost: {after}"
    );
    assert!(
        after.contains("/* tail-c */"),
        "tail-c comment lost: {after}"
    );
    assert!(after.contains("/// doc-a"), "doc-a comment lost: {after}");
}

/// I2: a no-op swap (identity) is byte-identical to the source — the
/// engine never mutates anything when fed the original block contents.
#[test]
fn engine_identity_swap_is_byte_identical() {
    let src = "\
    /// doc-a
    a: u32, // tail-a
    // outer-b
    b: u32,
    c: u32, /* tail-c */
";
    let span_a_start = src.find("/// doc-a").unwrap();
    let span_a = span_a_start..src[span_a_start..].find("u32").unwrap() + span_a_start + 3;
    let span_b_start = src.find("b: u32").unwrap();
    let span_b = span_b_start..span_b_start + 6;
    let span_c_start = src.find("c: u32").unwrap();
    let span_c = span_c_start..span_c_start + 6;

    let blocks = expand_blocks(src, 0..src.len(), &[span_a, span_b, span_c]).unwrap();
    let mut rw = SourceRewriter::new(src);
    for b in &blocks {
        let text = src[b.bytes.clone()].to_string();
        rw.replace(b.bytes.clone(), text);
    }
    assert_eq!(rw.finish().unwrap(), src);
}

/// I3: when nothing is replaced, `finish()` returns the source verbatim
/// and the rewriter reports `is_empty()`.
#[test]
fn engine_no_op_when_nothing_to_fix() {
    let src = "    a: u32,\n    b: u32,\n";
    let rw = SourceRewriter::new(src);
    assert!(rw.is_empty(), "I3: rewriter must report empty");
    assert_eq!(rw.finish().unwrap(), src, "I3: no-op must produce source");
}

/// I4: replacing a block with its own content does not mutate any byte
/// inside the block (the engine treats blocks as opaque blobs).
#[test]
fn engine_does_not_reformat_inside_blocks() {
    let src = "    a:   u32  ,  // weird spacing\n    b: u32,\n";
    let spans = vec![
        4..10,
        src.find("b: u32").unwrap()..src.find("b: u32").unwrap() + 6,
    ];
    let blocks = expand_blocks(src, 0..src.len(), &spans).unwrap();
    let original_a = src[blocks[0].bytes.clone()].to_string();
    let mut rw = SourceRewriter::new(src);
    rw.replace(blocks[0].bytes.clone(), original_a.clone());
    let out = rw.finish().unwrap();
    assert!(
        out.contains("a:   u32  ,  // weird spacing"),
        "I4 violated: engine reformatted block contents"
    );
}
