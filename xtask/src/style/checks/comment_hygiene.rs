use std::ops::Range;

use anyhow::Result;
use glob::Pattern;
use syn::{File, Item, spanned::Spanned, visit::Visit};

use super::{Check, Context};
use crate::{
    common::{
        fix::FixOutcome,
        parse::parse_file,
        violation::Violation,
        walker::{relative_to, workspace_rs_files_scoped},
    },
    style::config::CommentHygieneConfig,
};

pub(crate) const ID: &str = "comment_hygiene";

pub(crate) struct CommentHygiene;

impl Check for CommentHygiene {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.comment_hygiene;
        let excludes = compile_excludes(&cfg.exclude_paths);
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            if path_excluded(&excludes, &rel) {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            scan_file(cfg, &rel, &src, &file, &mut violations);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }

    fn fix(&self, ctx: &Context<'_>) -> Result<FixOutcome> {
        let cfg = &ctx.config.thresholds.comment_hygiene;
        let excludes = compile_excludes(&cfg.exclude_paths);
        let mut outcome = FixOutcome::default();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            if path_excluded(&excludes, &rel) {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let comments = scan_comments(&src);
            let macro_spans = collect_macro_spans(&file);
            let new_src = apply_category_fix(&src, &comments, &macro_spans, cfg);
            if let Some(new_src) = new_src {
                std::fs::write(&path, new_src)?;
                outcome.writes += 1;
            }
        }
        if outcome.writes == 0 {
            outcome.skipped.push(format!(
                "{ID}: size and density violations require manual review"
            ));
        }
        Ok(outcome)
    }
}

fn compile_excludes(patterns: &[String]) -> Vec<Pattern> {
    patterns
        .iter()
        .filter_map(|p| Pattern::new(p).ok())
        .collect()
}

fn path_excluded(patterns: &[Pattern], rel: &str) -> bool {
    patterns.iter().any(|p| p.matches(rel))
}

fn scan_file(
    cfg: &CommentHygieneConfig,
    rel: &str,
    src: &str,
    file: &File,
    out: &mut Vec<Violation>,
) {
    let comments = scan_comments(src);
    let macro_spans = collect_macro_spans(file);
    let fn_spans = collect_fn_spans(file);

    let visible: Vec<&Comment> = comments
        .iter()
        .filter(|c| !inside_any(c.byte_range.start, &macro_spans))
        .collect();

    detect_category(cfg, rel, &visible, out);
    detect_size(cfg, rel, &visible, out);
    detect_density(cfg, rel, &visible, &fn_spans, out);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommentKind {
    Line,
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocStyle {
    None,
    Outer,
    Inner,
}

#[derive(Clone, Debug)]
struct Comment {
    kind: CommentKind,
    doc_style: DocStyle,
    byte_range: Range<usize>,
    /// 1-based inclusive line numbers.
    line_start: usize,
    line_end: usize,
    /// Comment body without the opening `//`/`/*` and any closing `*/`,
    /// already trimmed of surrounding whitespace.
    body_trimmed: String,
}

fn scan_comments(src: &str) -> Vec<Comment> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let b = bytes[pos];
        match b {
            b'/' if peek(bytes, pos + 1) == Some(b'/') => {
                let comment = consume_line_comment(src, bytes, pos);
                pos = comment.byte_range.end;
                out.push(comment);
            }
            b'/' if peek(bytes, pos + 1) == Some(b'*') => {
                let comment = consume_block_comment(src, bytes, pos);
                pos = comment.byte_range.end;
                out.push(comment);
            }
            b'"' => pos = skip_string(bytes, pos),
            b'\'' => pos = skip_char_or_lifetime(bytes, pos),
            b'r' if at_token_start(bytes, pos)
                && matches!(peek(bytes, pos + 1), Some(b'#') | Some(b'"')) =>
            {
                pos = skip_raw_string(bytes, pos);
            }
            b'b' if at_token_start(bytes, pos) => {
                if let Some(next) = skip_byte_string_prefix(bytes, pos) {
                    pos = next;
                } else {
                    pos += 1;
                }
            }
            _ => pos = advance_one_char(src, pos),
        }
    }
    out
}

fn peek(bytes: &[u8], i: usize) -> Option<u8> {
    bytes.get(i).copied()
}

fn at_token_start(bytes: &[u8], pos: usize) -> bool {
    if pos == 0 {
        return true;
    }
    !is_ident_continue(bytes[pos - 1])
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn advance_one_char(src: &str, pos: usize) -> usize {
    let bytes = src.as_bytes();
    if !bytes[pos].is_ascii() {
        let mut next = pos + 1;
        while next < bytes.len() && !src.is_char_boundary(next) {
            next += 1;
        }
        next
    } else {
        pos + 1
    }
}

fn consume_line_comment(src: &str, bytes: &[u8], start: usize) -> Comment {
    let after_slashes = start + 2;
    let third = bytes.get(after_slashes).copied();
    let fourth = bytes.get(after_slashes + 1).copied();
    let doc_style = match third {
        Some(b'/') if fourth != Some(b'/') => DocStyle::Outer,
        Some(b'!') if fourth != Some(b'!') => DocStyle::Inner,
        _ => DocStyle::None,
    };
    let body_start = match doc_style {
        DocStyle::Outer | DocStyle::Inner => after_slashes + 1,
        DocStyle::None => after_slashes,
    };
    let mut end = body_start;
    while end < bytes.len() && bytes[end] != b'\n' {
        end += 1;
    }
    let body_trimmed = src[body_start..end].trim().to_string();
    let line = line_of(src, start);
    Comment {
        kind: CommentKind::Line,
        doc_style,
        byte_range: start..end,
        line_start: line,
        line_end: line,
        body_trimmed,
    }
}

fn consume_block_comment(src: &str, bytes: &[u8], start: usize) -> Comment {
    let after_slash_star = start + 2;
    let third = bytes.get(after_slash_star).copied();
    let fourth = bytes.get(after_slash_star + 1).copied();
    let doc_style = match third {
        Some(b'*') if fourth != Some(b'*') && fourth != Some(b'/') => DocStyle::Outer,
        Some(b'!') => DocStyle::Inner,
        _ => DocStyle::None,
    };
    let body_start = match doc_style {
        DocStyle::Outer | DocStyle::Inner => after_slash_star + 1,
        DocStyle::None => after_slash_star,
    };

    let mut depth = 1usize;
    let mut pos = body_start;
    while pos + 1 < bytes.len() && depth > 0 {
        if bytes[pos] == b'/' && bytes[pos + 1] == b'*' {
            depth += 1;
            pos += 2;
        } else if bytes[pos] == b'*' && bytes[pos + 1] == b'/' {
            depth -= 1;
            pos += 2;
        } else {
            pos += 1;
        }
    }
    if depth > 0 {
        pos = bytes.len();
    }
    let body_end = pos.saturating_sub(2).max(body_start);
    let body_trimmed = src
        .get(body_start..body_end)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let line_start = line_of(src, start);
    let line_end = line_of(src, pos.min(bytes.len()).saturating_sub(1));
    Comment {
        kind: CommentKind::Block,
        doc_style,
        byte_range: start..pos,
        line_start,
        line_end,
        body_trimmed,
    }
}

fn skip_string(bytes: &[u8], start: usize) -> usize {
    let mut pos = start + 1;
    while pos < bytes.len() {
        match bytes[pos] {
            b'\\' if pos + 1 < bytes.len() => pos += 2,
            b'"' => return pos + 1,
            _ => pos += 1,
        }
    }
    bytes.len()
}

fn skip_char_or_lifetime(bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    let body = start + 1;
    if body >= len {
        return len;
    }
    if bytes[body] == b'\\' {
        let scan_end = (body + 1 + 12).min(len);
        let mut p = body + 1;
        while p < scan_end && bytes[p] != b'\'' && bytes[p] != b'\n' {
            p += 1;
        }
        if p < scan_end && bytes[p] == b'\'' {
            return p + 1;
        }
        return body;
    }
    let scan_end = (body + 5).min(len);
    let mut p = body;
    while p < scan_end {
        if bytes[p] == b'\'' {
            return p + 1;
        }
        if bytes[p] == b'\n' {
            break;
        }
        p += 1;
    }
    body
}

fn skip_raw_string(bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    let mut p = start + 1;
    let mut hash_count = 0usize;
    while p < len && bytes[p] == b'#' {
        hash_count += 1;
        p += 1;
    }
    if p >= len || bytes[p] != b'"' {
        return start + 1;
    }
    p += 1;
    while p < len {
        if bytes[p] == b'"' {
            let end = p + 1 + hash_count;
            if end <= len && (1..=hash_count).all(|i| bytes[p + i] == b'#') {
                return end;
            }
        }
        p += 1;
    }
    len
}

fn skip_byte_string_prefix(bytes: &[u8], pos: usize) -> Option<usize> {
    let next = bytes.get(pos + 1)?;
    match next {
        b'"' => Some(skip_string(bytes, pos + 1)),
        b'r' if matches!(bytes.get(pos + 2), Some(b'#') | Some(b'"')) => {
            Some(skip_raw_string(bytes, pos + 1))
        }
        b'\'' => Some(skip_char_or_lifetime(bytes, pos + 1)),
        _ => None,
    }
}

fn line_of(src: &str, byte_offset: usize) -> usize {
    let upto = byte_offset.min(src.len());
    src[..upto].bytes().filter(|b| *b == b'\n').count() + 1
}

fn collect_macro_spans(file: &File) -> Vec<Range<usize>> {
    struct V {
        spans: Vec<Range<usize>>,
    }
    impl<'ast> Visit<'ast> for V {
        fn visit_item_macro(&mut self, m: &'ast syn::ItemMacro) {
            if m.mac.path.is_ident("macro_rules") {
                self.spans.push(m.mac.tokens.span().byte_range());
            }
            syn::visit::visit_item_macro(self, m);
        }
    }
    let mut v = V { spans: Vec::new() };
    v.visit_file(file);
    v.spans
}

fn inside_any(byte_offset: usize, spans: &[Range<usize>]) -> bool {
    spans
        .iter()
        .any(|s| byte_offset >= s.start && byte_offset < s.end)
}

#[derive(Clone, Debug)]
struct FnSpan {
    name: String,
    body_start_line: usize,
    body_end_line: usize,
    body_byte_range: Range<usize>,
}

fn collect_fn_spans(file: &File) -> Vec<FnSpan> {
    let mut out = Vec::new();
    walk_items_for_fns(&file.items, &mut Vec::new(), &mut out);
    out
}

fn walk_items_for_fns(items: &[Item], path: &mut Vec<String>, out: &mut Vec<FnSpan>) {
    for item in items {
        match item {
            Item::Fn(f) => out.push(fn_span_from_block(
                &qualified(path, &f.sig.ident.to_string()),
                &f.block,
            )),
            Item::Impl(im) => {
                for it in &im.items {
                    if let syn::ImplItem::Fn(method) = it {
                        out.push(fn_span_from_block(
                            &qualified(path, &method.sig.ident.to_string()),
                            &method.block,
                        ));
                    }
                }
            }
            Item::Trait(tr) => {
                for it in &tr.items {
                    if let syn::TraitItem::Fn(tf) = it
                        && let Some(block) = &tf.default
                    {
                        out.push(fn_span_from_block(
                            &qualified(path, &tf.sig.ident.to_string()),
                            block,
                        ));
                    }
                }
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    path.push(m.ident.to_string());
                    walk_items_for_fns(inner, path, out);
                    path.pop();
                }
            }
            _ => {}
        }
    }
}

fn qualified(path: &[String], name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{}::{name}", path.join("::"))
    }
}

fn fn_span_from_block(name: &str, block: &syn::Block) -> FnSpan {
    let span = block.brace_token.span;
    let open = span.open();
    let close = span.close();
    let body_byte_range = open.byte_range().start..close.byte_range().end;
    FnSpan {
        name: name.to_string(),
        body_start_line: open.start().line,
        body_end_line: close.end().line,
        body_byte_range,
    }
}

fn detect_category(
    cfg: &CommentHygieneConfig,
    rel: &str,
    comments: &[&Comment],
    out: &mut Vec<Violation>,
) {
    for c in comments {
        if c.doc_style != DocStyle::None {
            continue;
        }
        if has_allowed_marker(&c.body_trimmed, &cfg.allowed_inline_markers) {
            continue;
        }
        let key = format!("{rel}:{}:category", c.line_start);
        let preview = preview_body(&c.body_trimmed);
        let kind = match c.kind {
            CommentKind::Line => "line",
            CommentKind::Block => "block",
        };
        let msg = format!(
            "{kind} comment without allowed marker (`{preview}`); add a marker \
             ({}) or remove the comment",
            cfg.allowed_inline_markers.join(", "),
        );
        out.push(Violation::warn(ID, key, msg));
    }
}

fn has_allowed_marker(body: &str, markers: &[String]) -> bool {
    markers.iter().any(|m| body.starts_with(m.as_str()))
}

fn preview_body(body: &str) -> String {
    let single = body.replace('\n', " ");
    if single.chars().count() <= 60 {
        single
    } else {
        let truncated: String = single.chars().take(57).collect();
        format!("{truncated}…")
    }
}

fn detect_size(
    cfg: &CommentHygieneConfig,
    rel: &str,
    comments: &[&Comment],
    out: &mut Vec<Violation>,
) {
    let blocks = group_blocks(comments);
    for block in blocks {
        let lines = block.line_end - block.line_start + 1;
        let (limit, suffix, label) = match (block.kind, block.doc_style) {
            (_, DocStyle::Outer | DocStyle::Inner) => {
                (cfg.doc_block_max_lines, "size:doc", "doc-comment block")
            }
            (CommentKind::Line, DocStyle::None) => {
                (cfg.inline_max_lines, "size:inline", "inline `//` block")
            }
            (CommentKind::Block, DocStyle::None) => {
                (cfg.inline_max_lines, "size:inline", "inline `/* */` block")
            }
        };
        if lines > limit {
            let key = format!("{rel}:{}:{suffix}", block.line_start);
            let msg = format!(
                "{label} spans {lines} lines (limit {limit}); shorten or move long contracts \
                 to the owning crate `README.md`"
            );
            out.push(Violation::warn(ID, key, msg));
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Block {
    kind: CommentKind,
    doc_style: DocStyle,
    line_start: usize,
    line_end: usize,
}

fn group_blocks(comments: &[&Comment]) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    for c in comments {
        if c.kind == CommentKind::Block {
            blocks.push(Block {
                kind: c.kind,
                doc_style: c.doc_style,
                line_start: c.line_start,
                line_end: c.line_end,
            });
            continue;
        }
        if let Some(last) = blocks.last_mut()
            && last.kind == CommentKind::Line
            && last.doc_style == c.doc_style
            && c.line_start == last.line_end + 1
        {
            last.line_end = c.line_end;
            continue;
        }
        blocks.push(Block {
            kind: c.kind,
            doc_style: c.doc_style,
            line_start: c.line_start,
            line_end: c.line_end,
        });
    }
    blocks
}

fn detect_density(
    cfg: &CommentHygieneConfig,
    rel: &str,
    comments: &[&Comment],
    fn_spans: &[FnSpan],
    out: &mut Vec<Violation>,
) {
    for fs in fn_spans {
        let body_lines = fs.body_end_line.saturating_sub(fs.body_start_line) + 1;
        if body_lines < cfg.fn_density_min_body_lines {
            continue;
        }
        let mut comment_lines = 0usize;
        for c in comments {
            if c.doc_style != DocStyle::None {
                continue;
            }
            if !range_overlaps(&c.byte_range, &fs.body_byte_range) {
                continue;
            }
            comment_lines += c.line_end - c.line_start + 1;
        }
        if comment_lines == 0 {
            continue;
        }
        let scaled_pct = (comment_lines as u64 * 100) / body_lines as u64;
        if scaled_pct > u64::from(cfg.fn_density_threshold_pct) {
            let key = format!("{rel}::{}:density", fs.name);
            let limit_pct = cfg.fn_density_threshold_pct;
            let msg = format!(
                "fn `{}` has {comment_lines}/{body_lines} comment lines ({scaled_pct}%, limit \
                 {limit_pct}%); resolve category violations first or move WHY into a \
                 doc-block above the fn",
                fs.name
            );
            out.push(Violation::warn(ID, key, msg));
        }
    }
}

fn range_overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

fn apply_category_fix(
    src: &str,
    comments: &[Comment],
    macro_spans: &[Range<usize>],
    cfg: &CommentHygieneConfig,
) -> Option<String> {
    let mut targets: Vec<Range<usize>> = Vec::new();
    for c in comments {
        if c.doc_style != DocStyle::None {
            continue;
        }
        if has_allowed_marker(&c.body_trimmed, &cfg.allowed_inline_markers) {
            continue;
        }
        if inside_any(c.byte_range.start, macro_spans) {
            continue;
        }
        if !is_safe_to_autoremove(c) {
            continue;
        }
        targets.push(removal_range(src, &c.byte_range));
    }
    if targets.is_empty() {
        return None;
    }
    targets.sort_by_key(|r| std::cmp::Reverse(r.start));
    let mut buf = src.to_string();
    let mut last_start = usize::MAX;
    for r in targets {
        if r.end <= last_start {
            buf.replace_range(r.clone(), "");
            last_start = r.start;
        }
    }
    Some(buf)
}

/// A comment is safe to delete via autofix only when its body looks like trivial
/// restatement of nearby code, with no signal of external-contract documentation
/// (spec references, byte-format annotations, identifier mentions, version refs).
/// When in doubt, refuse: lost prose is irreversible — the user can always add a
/// marker manually for cases the autofix skips.
fn is_safe_to_autoremove(c: &Comment) -> bool {
    if c.kind == CommentKind::Block {
        return false;
    }
    if c.line_end != c.line_start {
        return false;
    }
    let body = c.body_trimmed.as_str();
    if body.is_empty() {
        return true;
    }
    if body.chars().count() > 30 {
        return false;
    }
    if has_value_signal(body) {
        return false;
    }
    true
}

fn has_value_signal(body: &str) -> bool {
    if body.chars().filter(char::is_ascii_uppercase).count() >= 2 {
        return true;
    }
    if body.chars().any(|ch| ch.is_ascii_digit()) {
        return true;
    }
    if body.contains('`') || body.contains('=') || body.contains('(') || body.contains(')') {
        return true;
    }
    if body.chars().skip(1).any(|ch| ch == ':') {
        return true;
    }
    let lower = body.to_ascii_lowercase();
    if lower.starts_with("see ")
        || lower.contains(" see ")
        || lower.starts_with("per ")
        || lower.contains(" per ")
        || lower.contains(" ref ")
        || lower.contains("rfc")
        || lower.contains("spec")
        || lower.contains("invariant")
    {
        return true;
    }
    false
}

fn removal_range(src: &str, comment: &Range<usize>) -> Range<usize> {
    let bytes = src.as_bytes();
    let mut start = comment.start;
    while start > 0 && matches!(bytes[start - 1], b' ' | b'\t') {
        start -= 1;
    }
    let line_start = src[..start].rfind('\n').map_or(0, |idx| idx + 1);
    let leading_only = src[line_start..start]
        .bytes()
        .all(|b| b == b' ' || b == b'\t');
    let mut end = comment.end;
    if leading_only && end < bytes.len() && bytes[end] == b'\n' {
        end += 1;
        return line_start..end;
    }
    start..comment.end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> CommentHygieneConfig {
        CommentHygieneConfig::default()
    }

    fn parse(src: &str) -> File {
        syn::parse_file(src).unwrap_or_else(|e| panic!("parse: {e}\n---\n{src}"))
    }

    fn run_all(src: &str) -> Vec<Violation> {
        let cfg = cfg();
        let file = parse(src);
        let mut out = Vec::new();
        scan_file(&cfg, "fixture.rs", src, &file, &mut out);
        out
    }

    fn keys(vs: &[Violation]) -> Vec<String> {
        vs.iter().map(|v| v.key.clone()).collect()
    }

    #[test]
    fn line_comment_doc_style_recognized() {
        let src = "/// outer\n//! inner\n// plain\n";
        let comments = scan_comments(src);
        assert_eq!(comments.len(), 3);
        assert_eq!(comments[0].doc_style, DocStyle::Outer);
        assert_eq!(comments[1].doc_style, DocStyle::Inner);
        assert_eq!(comments[2].doc_style, DocStyle::None);
    }

    #[test]
    fn four_slashes_is_not_doc() {
        let comments = scan_comments("//// quad\n");
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].doc_style, DocStyle::None);
    }

    #[test]
    fn block_doc_style_recognized() {
        let src = "/** outer */\n/*! inner */\n/* plain */\n";
        let comments = scan_comments(src);
        assert_eq!(comments.len(), 3);
        assert_eq!(comments[0].doc_style, DocStyle::Outer);
        assert_eq!(comments[1].doc_style, DocStyle::Inner);
        assert_eq!(comments[2].doc_style, DocStyle::None);
    }

    #[test]
    fn block_comment_nesting() {
        let src = "/* outer /* inner */ outer */ rest\n";
        let comments = scan_comments(src);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].byte_range.end, src.find("rest").unwrap() - 1);
    }

    #[test]
    fn comment_inside_string_is_ignored() {
        let comments = scan_comments(r#"let s = "// not a comment";"#);
        assert!(comments.is_empty());
    }

    #[test]
    fn comment_inside_raw_string_is_ignored() {
        let comments = scan_comments(r####"let s = r#"// also not"#;"####);
        assert!(comments.is_empty());
    }

    #[test]
    fn lifetime_does_not_break_lexer() {
        let comments = scan_comments("fn f<'a>(x: &'a u32) -> &'a u32 { // ok\n x }\n");
        assert_eq!(comments.len(), 1);
        assert!(comments[0].body_trimmed.starts_with("ok"));
    }

    #[test]
    fn char_literal_with_escape_skipped() {
        let comments = scan_comments(
            r"let c = '\n'; // tail
",
        );
        assert_eq!(comments.len(), 1);
        assert!(comments[0].body_trimmed.starts_with("tail"));
    }

    #[test]
    fn category_inline_without_marker_flagged() {
        let src = "fn f() {\n    // foo\n}\n";
        let vs = run_all(src);
        assert_eq!(keys(&vs), vec!["fixture.rs:2:category"]);
    }

    #[test]
    fn category_safety_marker_ok() {
        let src = "fn f() {\n    // SAFETY: invariant\n}\n";
        let vs = run_all(src);
        assert!(vs.is_empty(), "got: {vs:?}");
    }

    #[test]
    fn category_kithara_marker_ok() {
        let src = "fn f() {\n    let _ = 1; // kithara:cancel:owner\n}\n";
        let vs = run_all(src);
        assert!(vs.is_empty(), "got: {vs:?}");
    }

    #[test]
    fn doc_comments_never_category() {
        let src = "//! file inner\n/// outer\n/** outer-block */\nstruct S;\n";
        let vs: Vec<_> = run_all(src)
            .into_iter()
            .filter(|v| v.key.contains("category"))
            .collect();
        assert!(vs.is_empty(), "got: {vs:?}");
    }

    #[test]
    fn block_without_marker_flagged() {
        let src = "fn f() {\n    /* quick note */\n}\n";
        let vs = run_all(src);
        assert_eq!(keys(&vs), vec!["fixture.rs:2:category"]);
    }

    #[test]
    fn size_inline_block_too_long() {
        let src = "fn f() {\n    // SAFETY: l1\n    // l2\n    // l3\n    // l4\n}\n";
        let vs = run_all(src);
        let size_keys: Vec<_> = vs
            .into_iter()
            .filter(|v| v.key.contains(":size:inline"))
            .map(|v| v.key)
            .collect();
        assert_eq!(size_keys, vec!["fixture.rs:2:size:inline"]);
    }

    #[test]
    fn size_inline_block_at_limit_ok() {
        let src = "fn f() {\n    // SAFETY: l1\n    // l2\n    // l3\n}\n";
        let vs: Vec<_> = run_all(src)
            .into_iter()
            .filter(|v| v.key.contains("size"))
            .collect();
        assert!(vs.is_empty(), "got: {vs:?}");
    }

    #[test]
    fn size_doc_block_too_long() {
        let mut src = String::new();
        for i in 0..21 {
            src.push_str(&format!("/// line {i}\n"));
        }
        src.push_str("struct S;\n");
        let vs: Vec<_> = run_all(&src)
            .into_iter()
            .filter(|v| v.key.contains(":size:doc"))
            .collect();
        assert_eq!(vs.len(), 1, "got: {vs:?}");
    }

    #[test]
    fn size_doc_block_at_limit_ok() {
        let mut src = String::new();
        for i in 0..20 {
            src.push_str(&format!("/// line {i}\n"));
        }
        src.push_str("struct S;\n");
        let vs: Vec<_> = run_all(&src)
            .into_iter()
            .filter(|v| v.key.contains(":size:"))
            .collect();
        assert!(vs.is_empty(), "got: {vs:?}");
    }

    #[test]
    fn density_threshold_breached() {
        let src = "fn f() {\n    let _ = 1;\n    // SAFETY: a\n    // SAFETY: b\n    // SAFETY: c\n    // SAFETY: d\n    let _ = 2;\n    let _ = 3;\n}\n";
        let vs: Vec<_> = run_all(src)
            .into_iter()
            .filter(|v| v.key.contains(":density"))
            .collect();
        assert_eq!(vs.len(), 1, "expected density flag, got: {vs:?}");
        assert!(vs[0].key.ends_with("::f:density"));
    }

    #[test]
    fn density_short_fn_skipped() {
        let src = "fn f() {\n    // SAFETY: a\n    let _ = 1;\n}\n";
        let vs: Vec<_> = run_all(src)
            .into_iter()
            .filter(|v| v.key.contains(":density"))
            .collect();
        assert!(vs.is_empty(), "got: {vs:?}");
    }

    #[test]
    fn macro_rules_body_excluded() {
        let src =
            "macro_rules! m {\n    () => {\n        // not flagged inside macro body\n    };\n}\n";
        let vs = run_all(src);
        assert!(vs.is_empty(), "got: {vs:?}");
    }

    #[test]
    fn fix_removes_inline_without_marker() {
        let src = "fn f() {\n    // foo\n    let x = 1;\n}\n";
        let comments = scan_comments(src);
        let file = parse(src);
        let macros = collect_macro_spans(&file);
        let out = apply_category_fix(src, &comments, &macros, &cfg()).expect("fix");
        assert_eq!(out, "fn f() {\n    let x = 1;\n}\n");
    }

    #[test]
    fn fix_keeps_safety_marker() {
        let src = "fn f() {\n    // SAFETY: invariant\n    let x = 1;\n}\n";
        let comments = scan_comments(src);
        let file = parse(src);
        let macros = collect_macro_spans(&file);
        let out = apply_category_fix(src, &comments, &macros, &cfg());
        assert!(out.is_none());
    }

    #[test]
    fn fix_strips_trailing_comment() {
        let src = "fn f() {\n    let x = 5; // unused\n}\n";
        let comments = scan_comments(src);
        let file = parse(src);
        let macros = collect_macro_spans(&file);
        let out = apply_category_fix(src, &comments, &macros, &cfg()).expect("fix");
        assert_eq!(out, "fn f() {\n    let x = 5;\n}\n");
    }

    #[test]
    fn fix_idempotent() {
        let src = "fn f() {\n    // foo\n    let x = 1; // bar\n    // SAFETY: keep\n}\n";
        let file = parse(src);
        let macros = collect_macro_spans(&file);
        let comments = scan_comments(src);
        let after_first = apply_category_fix(src, &comments, &macros, &cfg()).expect("first pass");
        let comments_2 = scan_comments(&after_first);
        let file_2 = parse(&after_first);
        let macros_2 = collect_macro_spans(&file_2);
        let after_second = apply_category_fix(&after_first, &comments_2, &macros_2, &cfg());
        assert!(after_second.is_none(), "second pass should be a no-op");
    }

    #[test]
    fn fix_preserves_doc_blocks() {
        let src = "//! file doc\n/// outer doc\nfn f() { let _ = 1; }\n";
        let comments = scan_comments(src);
        let file = parse(src);
        let macros = collect_macro_spans(&file);
        let out = apply_category_fix(src, &comments, &macros, &cfg());
        assert!(out.is_none());
    }

    #[test]
    fn fix_handles_multiple_removals() {
        let src = "fn f() {\n    // a\n    let x = 1;\n    // b\n    let y = 2;\n    // c\n    let z = 3;\n}\n";
        let comments = scan_comments(src);
        let file = parse(src);
        let macros = collect_macro_spans(&file);
        let out = apply_category_fix(src, &comments, &macros, &cfg()).expect("fix");
        assert_eq!(
            out,
            "fn f() {\n    let x = 1;\n    let y = 2;\n    let z = 3;\n}\n"
        );
    }

    #[test]
    fn excluded_path_skipped_in_run() {
        let cfg = CommentHygieneConfig {
            exclude_paths: vec!["**/build.rs".to_string()],
            ..Default::default()
        };
        let excludes = compile_excludes(&cfg.exclude_paths);
        assert!(path_excluded(&excludes, "crates/foo/build.rs"));
        assert!(!path_excluded(&excludes, "crates/foo/src/lib.rs"));
    }

    fn run_fix(src: &str) -> Option<String> {
        let comments = scan_comments(src);
        let file = parse(src);
        let macros = collect_macro_spans(&file);
        apply_category_fix(src, &comments, &macros, &cfg())
    }

    #[test]
    fn fix_preserves_esds_descriptor_annotation() {
        let src = "fn f() {\n    // ES_Descriptor (tag, size)\n    let _ = 1;\n}\n";
        assert!(
            run_fix(src).is_none(),
            "ESDS-style byte-format annotation must be preserved"
        );
    }

    #[test]
    fn fix_preserves_oti_equals_annotation() {
        let src = "fn f() {\n    // OTI = MPEG-4 Audio\n    let _ = 1;\n}\n";
        assert!(
            run_fix(src).is_none(),
            "spec reference with `=` must be preserved"
        );
    }

    #[test]
    fn fix_preserves_byte_size_note() {
        let src = "fn f() {\n    // AvgBitrate (4 bytes)\n    let _ = 1;\n}\n";
        assert!(run_fix(src).is_none(), "byte-size note must be preserved");
    }

    #[test]
    fn fix_preserves_backtick_identifier_ref() {
        let src = "fn f() {\n    // `foo` field\n    let _ = 1;\n}\n";
        assert!(
            run_fix(src).is_none(),
            "backtick identifier ref must be preserved"
        );
    }

    #[test]
    fn fix_preserves_long_explanation() {
        let src = "fn f() {\n    // long explanation about why this dance is necessary\n    let _ = 1;\n}\n";
        assert!(
            run_fix(src).is_none(),
            "long comment (>30 chars) must be preserved"
        );
    }

    #[test]
    fn fix_preserves_rfc_reference() {
        let src = "fn f() {\n    // see RFC 6381\n    let _ = 1;\n}\n";
        assert!(run_fix(src).is_none(), "RFC reference must be preserved");
    }

    #[test]
    fn fix_preserves_spec_word() {
        let src = "fn f() {\n    // per spec\n    let _ = 1;\n}\n";
        assert!(run_fix(src).is_none(), "spec mention must be preserved");
    }

    #[test]
    fn fix_preserves_block_comment_always() {
        let src = "fn f() {\n    /* foo */\n    let _ = 1;\n}\n";
        assert!(
            run_fix(src).is_none(),
            "block comments must never be autoremoved"
        );
    }

    #[test]
    fn fix_preserves_caps_word() {
        let src = "fn f() {\n    // ESDS bytes\n    let _ = 1;\n}\n";
        assert!(
            run_fix(src).is_none(),
            "2+ consecutive caps must be preserved"
        );
    }

    #[test]
    fn fix_preserves_invariant_word() {
        let src = "fn f() {\n    // holds invariant\n    let _ = 1;\n}\n";
        assert!(
            run_fix(src).is_none(),
            "`invariant` reference must be preserved"
        );
    }

    #[test]
    fn fix_still_removes_short_lowercase_prose() {
        let src = "fn f() {\n    // initialize\n    let x = 1;\n}\n";
        let out = run_fix(src).expect("fix");
        assert_eq!(out, "fn f() {\n    let x = 1;\n}\n");
    }

    #[test]
    fn fix_still_removes_trailing_short_lowercase() {
        let src = "fn f() {\n    let x = 5; // unused\n}\n";
        let out = run_fix(src).expect("fix");
        assert_eq!(out, "fn f() {\n    let x = 5;\n}\n");
    }
}
