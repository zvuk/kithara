//! Block-range computation: expand a bare item span into a slice that owns
//! its leading and trailing trivia (doc-comments, outer line comments,
//! trailing comma, trailing line comment).
//!
//! The full contract — what counts as "leading", how blank lines act as
//! boundaries, and when a scope is rejected for autofix — lives in the
//! `README.md` next to this module.

use std::ops::Range;

/// Byte ranges describing one item plus its attached trivia.
///
/// `bytes` is the slice that should be moved as a single opaque blob when
/// the item is reordered. `item_bytes` points to the bare item span and is
/// kept around for diagnostics and ordering keys.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BlockRange {
    pub(crate) bytes: Range<usize>,
    pub(crate) item_bytes: Range<usize>,
}

/// Reasons the engine refuses to compute block ranges for a sequence.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ExpansionError {
    /// A line-comment was found between two items, separated from both by a
    /// blank line. Reordering would silently break its visual association.
    FloatingComment { line: usize, snippet: String },
    /// An item span fell outside the source text. Indicates a programming
    /// error in the caller (wrong source string, wrong span).
    SpanOutOfRange { range: Range<usize>, src_len: usize },
    /// Item spans were not given in source order.
    ItemsOutOfOrder,
}

/// Compute block ranges for a sequence of sibling items inside a single
/// container (a struct's named fields, a trait's associated items, an
/// impl's items, a struct-init expression's fields).
///
/// `scope_bytes` is the byte range of the *content* of the container —
/// e.g. for `struct S { a: u32, b: u32 }` it is the slice between the
/// outer braces, not including them. Trivia expansion is clamped to this
/// range so leading/trailing comments outside the container never leak
/// into a block.
///
/// `item_spans` must be in source order and must each lie entirely inside
/// `scope_bytes`.
pub(crate) fn expand_blocks(
    src: &str,
    scope_bytes: Range<usize>,
    item_spans: &[Range<usize>],
) -> Result<Vec<BlockRange>, ExpansionError> {
    let src_len = src.len();
    if scope_bytes.end > src_len {
        return Err(ExpansionError::SpanOutOfRange {
            range: scope_bytes,
            src_len,
        });
    }
    for span in item_spans {
        if span.end > src_len || span.start < scope_bytes.start || span.end > scope_bytes.end {
            return Err(ExpansionError::SpanOutOfRange {
                range: span.clone(),
                src_len,
            });
        }
    }
    for pair in item_spans.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(ExpansionError::ItemsOutOfOrder);
        }
    }

    let mut blocks: Vec<BlockRange> = Vec::with_capacity(item_spans.len());

    for (i, span) in item_spans.iter().enumerate() {
        let lower = if i == 0 {
            scope_bytes.start
        } else {
            // Lower bound is the trailing-trivia end of the previous item;
            // we'll patch it below once we know it. For now use the previous
            // item's bare end.
            item_spans[i - 1].end
        };
        let upper = if i + 1 < item_spans.len() {
            item_spans[i + 1].start
        } else {
            scope_bytes.end
        };
        let leading_start = expand_leading(src, span.start, lower);
        let trailing_end = expand_trailing(src, span.end, upper);
        blocks.push(BlockRange {
            bytes: leading_start..trailing_end,
            item_bytes: span.clone(),
        });
    }

    // Detect floating comments in the gaps that did not get absorbed by
    // either neighbour's expansion.
    for i in 0..blocks.len() {
        let gap_start = if i == 0 {
            scope_bytes.start
        } else {
            blocks[i - 1].bytes.end
        };
        let gap_end = blocks[i].bytes.start;
        check_no_floating_comment(src, gap_start..gap_end)?;
    }
    if let Some(last) = blocks.last() {
        check_no_floating_comment(src, last.bytes.end..scope_bytes.end)?;
    }

    Ok(blocks)
}

/// Walk backwards from `item_start` absorbing contiguous outer comments
/// that immediately precede the item (no blank line between them and the
/// item). Stops at `lower_bound`.
fn expand_leading(src: &str, item_start: usize, lower_bound: usize) -> usize {
    let bytes = src.as_bytes();
    let mut cursor = item_start;

    loop {
        // Skip whitespace (incl. exactly one newline) immediately before cursor.
        let after_ws = skip_whitespace_back(bytes, cursor, lower_bound);
        if after_ws == cursor {
            return cursor;
        }
        let newlines = count_newlines(&bytes[after_ws..cursor]);
        if newlines > 1 {
            // Blank line ⇒ trivia boundary; do not absorb.
            return cursor;
        }

        let line = preceding_line(bytes, after_ws, lower_bound);
        let line_text = std::str::from_utf8(&bytes[line.start..line.end]).unwrap_or("");
        let trimmed = line_text.trim_start();
        // `///` and `//!` are doc-comments — syntactically attributes, so
        // they live inside the item's span if the caller passed a complete
        // span. Refuse to absorb them: doing so would either duplicate
        // them (bad) or hide a caller bug (worse).
        if trimmed.starts_with("///") || trimmed.starts_with("//!") {
            return cursor;
        }
        if trimmed.starts_with("//") || is_block_comment_only(trimmed) {
            cursor = line.start;
            continue;
        }
        return cursor;
    }
}

/// Walk forwards from `item_end` absorbing trailing comma and any inline
/// `// ...` comment on the same line. Stops before the next non-trivia
/// content but never crosses `upper_bound`.
fn expand_trailing(src: &str, item_end: usize, upper_bound: usize) -> usize {
    let bytes = src.as_bytes();
    let mut cursor = item_end;

    cursor = skip_inline_whitespace(bytes, cursor, upper_bound);
    if cursor < upper_bound && bytes[cursor] == b',' {
        cursor += 1;
    }
    cursor = skip_inline_whitespace(bytes, cursor, upper_bound);

    if cursor + 1 < upper_bound && bytes[cursor] == b'/' && bytes[cursor + 1] == b'/' {
        // Inline line-comment: absorb up to (but not including) the newline.
        let mut c = cursor + 2;
        while c < upper_bound && bytes[c] != b'\n' {
            c += 1;
        }
        cursor = c;
    } else if cursor + 1 < upper_bound && bytes[cursor] == b'/' && bytes[cursor + 1] == b'*' {
        // Inline block comment: absorb until the closing `*/`, but only if
        // that closing tag is on the same line as the item end (otherwise
        // the comment leaks across lines and probably belongs to whatever
        // follows it, not this item).
        let mut c = cursor + 2;
        let mut closed_inline = false;
        while c + 1 < upper_bound {
            if bytes[c] == b'\n' {
                break;
            }
            if bytes[c] == b'*' && bytes[c + 1] == b'/' {
                c += 2;
                closed_inline = true;
                break;
            }
            c += 1;
        }
        if closed_inline {
            cursor = c;
        }
    }

    cursor
}

fn check_no_floating_comment(src: &str, gap: Range<usize>) -> Result<(), ExpansionError> {
    if gap.start >= gap.end {
        return Ok(());
    }
    let slice = &src[gap.clone()];
    let mut line_start_byte = gap.start;
    for line_text in slice.split_inclusive('\n') {
        let trimmed = line_text.trim_start();
        let starts_comment = trimmed.starts_with("//") || trimmed.starts_with("/*");
        if starts_comment {
            // Compute the 1-based line number relative to the entire source.
            let line_no = src[..line_start_byte]
                .bytes()
                .filter(|b| *b == b'\n')
                .count()
                + 1;
            let snippet = trimmed.trim_end_matches('\n').trim_end().to_string();
            return Err(ExpansionError::FloatingComment {
                line: line_no,
                snippet,
            });
        }
        line_start_byte += line_text.len();
    }
    Ok(())
}

fn skip_whitespace_back(bytes: &[u8], from: usize, lower: usize) -> usize {
    let mut i = from;
    while i > lower {
        let prev = bytes[i - 1];
        if prev == b' ' || prev == b'\t' || prev == b'\n' || prev == b'\r' {
            i -= 1;
        } else {
            break;
        }
    }
    i
}

fn skip_inline_whitespace(bytes: &[u8], from: usize, upper: usize) -> usize {
    let mut i = from;
    while i < upper {
        let c = bytes[i];
        if c == b' ' || c == b'\t' {
            i += 1;
        } else {
            break;
        }
    }
    i
}

fn count_newlines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|b| **b == b'\n').count()
}

/// Range of the line that contains byte position `pos` (or the line ending
/// at `pos` if `pos` sits at a newline). Bounded by `lower`.
fn preceding_line(bytes: &[u8], pos: usize, lower: usize) -> Range<usize> {
    // pos points just past the last whitespace; scan backwards to find the
    // start of the line containing pos-1, and forward to find its end.
    let mut start = pos;
    while start > lower && bytes[start - 1] != b'\n' {
        start -= 1;
    }
    let mut end = pos;
    while end < bytes.len() && bytes[end] != b'\n' {
        end += 1;
    }
    if end < bytes.len() {
        end += 1; // include the newline
    }
    start..end
}

fn is_block_comment_only(line: &str) -> bool {
    let line = line.trim_end();
    line.starts_with("/*") && line.ends_with("*/")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Find byte ranges of substrings using `<<` and `>>` markers in the
    /// fixture string. Returns (`cleaned_src`, `item_spans`). Scope bytes cover
    /// the entire cleaned source so leading and trailing trivia outside the
    /// items can be exercised end-to-end.
    fn parse_markers(input: &str) -> (String, Vec<Range<usize>>) {
        let mut out = String::with_capacity(input.len());
        let mut spans = Vec::new();
        let mut chars = input.chars().peekable();
        let mut current_start: Option<usize> = None;
        while let Some(c) = chars.next() {
            if c == '<' && chars.peek() == Some(&'<') {
                chars.next();
                current_start = Some(out.len());
                continue;
            }
            if c == '>' && chars.peek() == Some(&'>') {
                chars.next();
                let start = current_start.take().expect("unbalanced markers");
                spans.push(start..out.len());
                continue;
            }
            out.push(c);
        }
        assert!(
            current_start.is_none(),
            "unclosed << marker in fixture input"
        );
        (out, spans)
    }

    fn run(input: &str) -> Result<(String, Vec<BlockRange>), ExpansionError> {
        let (src, spans) = parse_markers(input);
        let scope = 0..src.len();
        let blocks = expand_blocks(&src, scope, &spans)?;
        Ok((src, blocks))
    }

    fn block_strs<'a>(src: &'a str, blocks: &[BlockRange]) -> Vec<&'a str> {
        blocks.iter().map(|b| &src[b.bytes.clone()]).collect()
    }

    #[test]
    fn bare_item_no_trivia() {
        let (src, blocks) = run("    <<a: u32>>,\n    <<b: u32>>,\n").unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(&src[blocks[0].bytes.clone()], "a: u32,");
        assert_eq!(&src[blocks[1].bytes.clone()], "b: u32,");
    }

    #[test]
    fn doc_comment_already_in_item_span_is_left_alone() {
        // `///` doc comments are syntactically attributes; if the caller passes
        // an item span that includes them, expand_leading just returns it.
        let input = "    <<    /// docs\n    a: u32>>,\n    <<b: u32>>,\n";
        let (src, blocks) = run(input).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            &src[blocks[0].bytes.clone()],
            "    /// docs\n    a: u32,",
            "expansion absorbed leading whitespace and trailing comma"
        );
    }

    #[test]
    fn outer_line_comment_no_blank_line_is_absorbed() {
        let input = "    // explain a\n    <<a: u32>>,\n    <<b: u32>>,\n";
        let (src, blocks) = run(input).unwrap();
        assert_eq!(
            &src[blocks[0].bytes.clone()],
            "    // explain a\n    a: u32,",
        );
        assert_eq!(&src[blocks[1].bytes.clone()], "b: u32,");
    }

    #[test]
    fn outer_line_comment_with_blank_line_is_floating() {
        // Comment is separated from `a` by a blank line ⇒ floating ⇒ error.
        let input = "    // floating\n\n    <<a: u32>>,\n    <<b: u32>>,\n";
        let err = run(input).unwrap_err();
        assert!(
            matches!(err, ExpansionError::FloatingComment { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn multiple_outer_comments_absorbed_as_one_block() {
        let input = "    // first\n    // second\n    <<a: u32>>,\n    <<b: u32>>,\n";
        let (src, blocks) = run(input).unwrap();
        assert_eq!(
            &src[blocks[0].bytes.clone()],
            "    // first\n    // second\n    a: u32,",
        );
    }

    #[test]
    fn outer_block_comment_absorbed() {
        let input = "    /* hint */\n    <<a: u32>>,\n    <<b: u32>>,\n";
        let (src, blocks) = run(input).unwrap();
        assert_eq!(&src[blocks[0].bytes.clone()], "    /* hint */\n    a: u32,",);
    }

    #[test]
    fn trailing_comma_absorbed() {
        let input = "    <<a: u32>>,\n    <<b: u32>>,\n";
        let (src, blocks) = run(input).unwrap();
        assert_eq!(&src[blocks[0].bytes.clone()], "a: u32,");
    }

    #[test]
    fn trailing_inline_comment_absorbed() {
        let input = "    <<a: u32>>, // inline\n    <<b: u32>>,\n";
        let (src, blocks) = run(input).unwrap();
        assert_eq!(&src[blocks[0].bytes.clone()], "a: u32, // inline");
        assert_eq!(&src[blocks[1].bytes.clone()], "b: u32,");
    }

    #[test]
    fn trailing_inline_comment_without_comma() {
        let input = "    <<a: u32>> // bare\n    <<b: u32>>\n";
        let (src, blocks) = run(input).unwrap();
        assert_eq!(&src[blocks[0].bytes.clone()], "a: u32 // bare");
    }

    #[test]
    fn floating_comment_between_items_rejected() {
        let input = "    <<a: u32>>,\n\n    // strays\n\n    <<b: u32>>,\n";
        let err = run(input).unwrap_err();
        match err {
            ExpansionError::FloatingComment { snippet, .. } => {
                assert!(snippet.contains("strays"), "got snippet {snippet:?}");
            }
            other => panic!("expected FloatingComment, got {other:?}"),
        }
    }

    #[test]
    fn floating_comment_after_last_item_rejected() {
        let input = "    <<a: u32>>,\n    <<b: u32>>,\n\n    // trailing strays\n\n";
        let err = run(input).unwrap_err();
        assert!(matches!(err, ExpansionError::FloatingComment { .. }));
    }

    #[test]
    fn item_span_outside_src_is_error() {
        // 0..100 cannot exist in a 5-byte source.
        let span = 0..100;
        let err = expand_blocks("hello", 0..5, std::slice::from_ref(&span)).unwrap_err();
        assert!(matches!(err, ExpansionError::SpanOutOfRange { .. }));
    }

    #[test]
    fn out_of_order_items_rejected() {
        let src = "a b c d";
        let err = expand_blocks(src, 0..7, &[2..3, 0..1]).unwrap_err();
        assert_eq!(err, ExpansionError::ItemsOutOfOrder);
    }

    #[test]
    fn idempotent_expansion() {
        // Running expansion twice on the same input gives the same result.
        let input = "    // hint\n    <<a: u32>>, // tail\n    <<b: u32>>,\n";
        let (src, blocks_a) = run(input).unwrap();
        let (_, blocks_b) = run(input).unwrap();
        assert_eq!(blocks_a, blocks_b);
        assert_eq!(block_strs(&src, &blocks_a), block_strs(&src, &blocks_b));
    }

    #[test]
    fn blocks_cover_disjoint_ranges() {
        // Both items use plain outer line comments (// not ///). Doc
        // comments belong to the item span itself; passing a marker that
        // excludes a leading /// is a caller bug and will be rejected as
        // a floating comment (covered separately).
        let input = "    // a-doc\n    <<a: u32>>,\n    // b-doc\n    <<b: u32>>,\n";
        let (_, blocks) = run(input).unwrap();
        assert!(blocks[0].bytes.end <= blocks[1].bytes.start);
    }

    #[test]
    fn doc_comment_outside_item_span_is_floating_error() {
        // If the caller hands us an item span that does not include the
        // attached `///`, we refuse to magically absorb it — the expansion
        // detects it as a floating comment.
        let input = "    /// b-doc\n    <<b: u32>>,\n";
        let err = run(input).unwrap_err();
        assert!(matches!(err, ExpansionError::FloatingComment { .. }));
    }
}
