//! Fields in `Foo { ... }` literal expressions should follow:
//!
//! 1. Shorthand fields (`field_a` without `: value`) come first.
//! 2. Explicit fields (`field_b: value`) come after.
//! 3. Within each bucket, relative order is unconstrained — clippy's
//!    `inconsistent_struct_constructor` already enforces "match struct
//!    definition order" for all-shorthand inits, and we let the developer
//!    pick the most readable layout otherwise.
//! 4. The optional `..base` rest comes last (Rust syntax already enforces this).
//!
//! Tuple-struct expressions (`Foo(a, b)`) and explicit-position struct exprs
//! (`Foo { 0: a, 1: b }`) are skipped — they have no name to sort by.

use std::{cmp::Ordering, collections::HashSet, ops::Range};

use anyhow::Result;
use proc_macro2::Span;
use syn::{
    ExprStruct, FieldValue, Member,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::{
    common::{
        fix::{ExpansionError, FixOutcome, SourceRewriter, expand_blocks},
        parse::parse_file,
        violation::Violation,
        walker::{relative_to, workspace_rs_files_scoped},
    },
    style::config::StructInitOrderConfig,
};

pub(crate) const ID: &str = "struct_init_order";

pub(crate) struct StructInitOrder;

impl Check for StructInitOrder {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.struct_init_order;
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");

            let mut v = InitVisitor {
                cfg,
                rel: &rel,
                out: &mut violations,
            };
            v.visit_file(&file);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }

    fn fix(&self, ctx: &Context<'_>) -> Result<FixOutcome> {
        let cfg = &ctx.config.thresholds.struct_init_order;
        let mut outcome = FixOutcome::default();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(src) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(file) = syn::parse_file(&src) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            let mut rw = SourceRewriter::new(&src);
            let mut visitor = FixVisitor {
                cfg,
                rel: &rel,
                src: &src,
                rw: &mut rw,
                skipped: &mut outcome.skipped,
            };
            visitor.visit_file(&file);
            if !rw.is_empty() {
                let new_src = rw.finish()?;
                std::fs::write(&path, new_src)?;
                outcome.writes += 1;
            }
        }
        Ok(outcome)
    }
}

struct FixVisitor<'a, 'src> {
    cfg: &'a StructInitOrderConfig,
    rel: &'a str,
    src: &'src str,
    rw: &'a mut SourceRewriter<'src>,
    skipped: &'a mut Vec<String>,
}

impl<'ast> Visit<'ast> for FixVisitor<'_, '_> {
    fn visit_expr_struct(&mut self, e: &'ast ExprStruct) {
        if let Err(reason) = try_fix_expr_struct(self.cfg, self.src, e, self.rw) {
            self.skipped.push(format!(
                "{}:{}: {reason}",
                self.rel,
                e.path.span().start().line
            ));
        }
        visit::visit_expr_struct(self, e);
    }
}

/// Reorder one `Foo { ... }` literal in place. Returns `Ok(())` whether
/// the literal was already ordered, was patched, or was a no-op; returns
/// `Err(reason)` only when the engine refused (floating comment, missing
/// trailing comma, `..base` rest, etc.) so the caller can log it.
fn try_fix_expr_struct(
    cfg: &StructInitOrderConfig,
    src: &str,
    e: &ExprStruct,
    rw: &mut SourceRewriter<'_>,
) -> Result<(), String> {
    if e.fields.len() < 2 {
        return Ok(());
    }
    if e.dot2_token.is_some() || e.rest.is_some() {
        // `..base` rests aren't reordered; we'd need to keep them last
        // and shuffle only the named fields, which is doable but adds
        // surface area for marginal value — skip for now.
        return Err("contains `..base` rest".to_string());
    }

    // Detect use-def conflicts: a shorthand field `x` evaluates as
    // moving/borrowing the binding `x` from the surrounding scope. If
    // any explicit field's expression also references `x` (e.g.
    // `Self { spec: pcm.spec(), pcm }`), reordering shorthand to come
    // first would move `pcm` before `pcm.spec()` runs — a compile
    // error. Skip the literal in that case; the developer can either
    // rewrite to fully-explicit form or live with the report.
    if has_shorthand_use_def_conflict(&e.fields) {
        return Err("shorthand field is moved before another field reads it".to_string());
    }

    let actual: Vec<InitKey> = e
        .fields
        .iter()
        .enumerate()
        .map(|(idx, fv)| InitKey {
            idx,
            ..classify(cfg, fv)
        })
        .collect();
    let mut expected = actual.clone();
    expected.sort_by(cmp_init_key);
    if actual
        .iter()
        .map(|k| k.idx)
        .eq(expected.iter().map(|k| k.idx))
    {
        return Ok(()); // already in expected order
    }

    let scope_start = e.brace_token.span.open().byte_range().end;
    let scope_end = e.brace_token.span.close().byte_range().start;

    let item_spans: Vec<Range<usize>> = e.fields.iter().map(|fv| fv.span().byte_range()).collect();

    let blocks = match expand_blocks(src, scope_start..scope_end, &item_spans) {
        Ok(b) => b,
        Err(ExpansionError::FloatingComment { line, snippet }) => {
            return Err(format!("floating comment at line {line}: `{snippet}`"));
        }
        Err(other) => return Err(format!("engine error: {other:?}")),
    };

    // Require a trailing comma on the last block. If absent, swapping
    // would either drop a comma or stitch two items together. Defer to
    // a future cargo-fmt run that adds the trailing comma, or to a
    // manual fix.
    let last_block_text = &src[blocks.last().expect("non-empty").bytes.clone()];
    if !last_block_text.trim_end().ends_with(',')
        && !last_block_text.trim_end().ends_with([')', '}', ']'])
    {
        // Re-check: did expand_trailing actually pick up a comma?
        let last = blocks.last().expect("non-empty");
        if !src[last.item_bytes.end..last.bytes.end].contains(',') {
            return Err("last field has no trailing comma".to_string());
        }
    }

    let texts: Vec<String> = blocks
        .iter()
        .map(|b| src[b.bytes.clone()].to_string())
        .collect();
    for (slot_idx, expected_key) in expected.iter().enumerate() {
        let source_idx = expected_key.idx;
        if source_idx == slot_idx {
            continue;
        }
        rw.replace(blocks[slot_idx].bytes.clone(), texts[source_idx].clone());
    }
    Ok(())
}

struct InitVisitor<'a> {
    cfg: &'a StructInitOrderConfig,
    rel: &'a str,
    out: &'a mut Vec<Violation>,
}

impl<'ast> Visit<'ast> for InitVisitor<'_> {
    fn visit_expr_struct(&mut self, e: &'ast ExprStruct) {
        check_expr_struct(self.cfg, self.rel, e, self.out);
        visit::visit_expr_struct(self, e);
    }
}

#[derive(Debug, Clone)]
struct InitKey {
    idx: usize,
    /// 0 = shorthand, 1 = explicit, 2 = unnamed/positional (sorts last).
    bucket: usize,
    name: String,
}

fn cmp_init_key(a: &InitKey, b: &InitKey) -> Ordering {
    a.bucket
        .cmp(&b.bucket)
        // Stable within a bucket: keep the source-position order, so we don't
        // collide with clippy's `inconsistent_struct_constructor` (which
        // demands the all-shorthand case match struct definition order).
        .then_with(|| a.idx.cmp(&b.idx))
}

fn classify(cfg: &StructInitOrderConfig, fv: &FieldValue) -> InitKey {
    let is_shorthand = fv.colon_token.is_none();
    let bucket = match &fv.member {
        Member::Unnamed(_) => 2,
        Member::Named(_) if is_shorthand && cfg.shorthand_first => 0,
        Member::Named(_) => 1,
    };
    let name = match &fv.member {
        Member::Named(id) => id.to_string(),
        Member::Unnamed(i) => i.index.to_string(),
    };
    InitKey {
        idx: 0,
        bucket,
        name,
    }
}

fn check_expr_struct(
    cfg: &StructInitOrderConfig,
    rel: &str,
    e: &ExprStruct,
    out: &mut Vec<Violation>,
) {
    if e.fields.len() < 2 {
        return;
    }
    let actual: Vec<InitKey> = e
        .fields
        .iter()
        .enumerate()
        .map(|(idx, fv)| InitKey {
            idx,
            ..classify(cfg, fv)
        })
        .collect();

    let mut expected = actual.clone();
    expected.sort_by(cmp_init_key);

    if actual
        .iter()
        .map(|k| k.idx)
        .eq(expected.iter().map(|k| k.idx))
    {
        return;
    }

    let type_name = e
        .path
        .segments
        .last()
        .map_or_else(|| "<anon>".to_string(), |s| s.ident.to_string());
    let line = type_span_start_line(e);
    let key = format!("{rel}:{line}::{type_name}");
    let actual_summary = actual
        .iter()
        .map(|k| k.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let expected_summary = expected
        .iter()
        .map(|k| k.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let msg = format!(
        "init `{type_name} {{ ... }}` should put shorthand fields before explicit ones: \
         expected [{expected_summary}], found [{actual_summary}]"
    );
    out.push(Violation::warn(ID, key, msg));
}

/// True iff some shorthand field name is referenced by another field's
/// explicit expression. In that case, moving the shorthand first would
/// move/borrow the value before the explicit expression runs, which is
/// a compile error or behaviour change. The check is conservative: it
/// only inspects identifier references, not field-access paths or
/// method-call receivers (those start with the same identifier).
fn has_shorthand_use_def_conflict(
    fields: &syn::punctuated::Punctuated<FieldValue, syn::Token![,]>,
) -> bool {
    let shorthand_names: HashSet<String> = fields
        .iter()
        .filter(|fv| fv.colon_token.is_none())
        .filter_map(|fv| match &fv.member {
            Member::Named(id) => Some(id.to_string()),
            Member::Unnamed(_) => None,
        })
        .collect();
    if shorthand_names.is_empty() {
        return false;
    }
    for fv in fields.iter().filter(|fv| fv.colon_token.is_some()) {
        let mut v = IdentScanner {
            names: &shorthand_names,
            found: false,
        };
        v.visit_expr(&fv.expr);
        if v.found {
            return true;
        }
    }
    false
}

struct IdentScanner<'a> {
    names: &'a HashSet<String>,
    found: bool,
}

impl<'ast> Visit<'ast> for IdentScanner<'_> {
    fn visit_path(&mut self, p: &'ast syn::Path) {
        // Match a leading single-segment ident: `foo`, `foo.bar()`,
        // `foo[0]`, etc. all parse with `foo` as the path's first
        // segment. Multi-segment paths like `Self::new()` or
        // `crate::foo` cannot reference a local binding, so skip.
        if let Some(first) = p.segments.first()
            && p.leading_colon.is_none()
            && self.names.contains(&first.ident.to_string())
        {
            self.found = true;
            return;
        }
        visit::visit_path(self, p);
    }
}

fn type_span_start_line(e: &ExprStruct) -> usize {
    let span = e
        .path
        .segments
        .first()
        .map_or_else(Span::call_site, Spanned::span);
    span.start().line
}

#[cfg(test)]
mod fix_tests {
    use std::collections::BTreeMap;

    use syn::visit::Visit;

    use super::*;

    fn default_cfg() -> StructInitOrderConfig {
        StructInitOrderConfig {
            shorthand_first: true,
        }
    }

    /// Apply the visitor-driven fix to a snippet wrapped in a fn body so
    /// `syn::parse_file` accepts it. Returns the rewritten snippet
    /// (still wrapped) and the list of skip reasons collected.
    fn run_fix(src: &str) -> (String, Vec<String>) {
        let cfg = default_cfg();
        let file = syn::parse_file(src).unwrap_or_else(|e| panic!("parse failed: {e}\n---\n{src}"));
        let mut rw = SourceRewriter::new(src);
        let mut skipped = Vec::new();
        let mut visitor = FixVisitor {
            cfg: &cfg,
            rel: "fixture.rs",
            src,
            rw: &mut rw,
            skipped: &mut skipped,
        };
        visitor.visit_file(&file);
        let out = if rw.is_empty() {
            src.to_string()
        } else {
            rw.finish().expect("rewriter finish")
        };
        (out, skipped)
    }

    /// Count line-comment occurrences as a multiset (I1 invariant check).
    fn comment_multiset(src: &str) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for line in src.lines() {
            let t = line.trim_start();
            if t.starts_with("//") {
                *counts.entry(t.trim_end().to_string()).or_insert(0) += 1;
            }
        }
        counts
    }

    #[test]
    fn shorthand_after_explicit_is_swapped() {
        let src = "fn main() { let _ = Foo { x: 1, y, }; }";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(
            out.contains("Foo { y, x: 1, }"),
            "expected shorthand first, got: {out}"
        );
    }

    #[test]
    fn already_ordered_is_no_op() {
        let src = "fn main() { let _ = Foo { y, x: 1, }; }";
        let (out, _) = run_fix(src);
        assert_eq!(out, src);
    }

    #[test]
    fn single_field_is_no_op() {
        let src = "fn main() { let _ = Foo { x: 1, }; }";
        let (out, _) = run_fix(src);
        assert_eq!(out, src);
    }

    #[test]
    fn rest_base_is_skipped() {
        let src = "fn main() { let base = Foo::default(); let _ = Foo { x: 1, y, ..base }; }";
        let (out, skipped) = run_fix(src);
        assert_eq!(out, src, "must not modify");
        assert!(
            skipped.iter().any(|s| s.contains("..base")),
            "skipped: {skipped:?}"
        );
    }

    #[test]
    fn comments_move_with_their_field() {
        let src = "\
fn main() {
    let _ = Foo {
        // doc for x
        x: 1,
        y,
    };
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        // I1: comment multiset preserved.
        assert_eq!(comment_multiset(src), comment_multiset(&out));
        // The comment must have followed `x` to its new position.
        let y_pos = out.find("y,").expect("y,");
        let x_pos = out.find("x: 1,").expect("x: 1,");
        let comment_pos = out.find("// doc for x").expect("comment");
        assert!(
            y_pos < comment_pos && comment_pos < x_pos,
            "comment did not move with x:\n{out}"
        );
    }

    #[test]
    fn floating_comment_is_skipped() {
        // The comments below have a blank line on BOTH sides — they're
        // truly floating and the engine refuses to touch this scope.
        let src = "\
fn main() {
    let _ = Foo {
        x: 1,

        // floating comment
        // another

        y,
    };
}
";
        let (out, skipped) = run_fix(src);
        assert_eq!(out, src, "must not modify when comments are floating");
        assert!(
            skipped.iter().any(|s| s.contains("floating")),
            "skipped: {skipped:?}"
        );
    }

    #[test]
    fn comment_attached_to_next_field_is_carried() {
        // Blank line BEFORE the comments, but NO blank line between the
        // comments and `y`. They glue to `y` and travel with it.
        let src = "\
fn main() {
    let _ = Foo {
        x: 1,

        // glued to y
        // also glued
        y,
    };
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        // I1: comment multiset preserved.
        assert_eq!(comment_multiset(src), comment_multiset(&out));
        // After the swap, `y` (and its comments) come first.
        let comment_pos = out.find("// glued to y").expect("comment");
        let x_pos = out.find("x: 1,").expect("x: 1,");
        assert!(comment_pos < x_pos, "comment did not travel with y:\n{out}");
    }

    #[test]
    fn missing_trailing_comma_is_skipped() {
        // `Foo { x: 1, y }` — no trailing comma after `y`. Skipped to
        // avoid stitching two items together when reordering.
        let src = "fn main() { let _ = Foo { x: 1, y }; }";
        let (out, skipped) = run_fix(src);
        assert_eq!(out, src);
        assert!(
            skipped.iter().any(|s| s.contains("trailing comma")),
            "skipped: {skipped:?}"
        );
    }

    #[test]
    fn idempotent_run() {
        // Run fix twice; second run is a no-op.
        let src = "fn main() { let _ = Foo { x: 1, y, z: 2, }; }";
        let (after_first, skipped1) = run_fix(src);
        assert!(skipped1.is_empty());
        let (after_second, skipped2) = run_fix(&after_first);
        assert_eq!(after_first, after_second, "I2: idempotency violated");
        assert!(skipped2.is_empty());
    }

    #[test]
    fn shorthand_use_def_conflict_is_skipped() {
        // `pcm` would be moved by the shorthand field; reordering it
        // first would break `spec: pcm.spec()` which still needs the
        // borrow. Skipped to keep the source compiling.
        let src = "\
fn make(pcm: Pcm) -> Self {
    Self {
        spec: pcm.spec(),
        pcm,
    }
}
";
        let (out, skipped) = run_fix(src);
        assert_eq!(out, src, "must not reorder when a use-def conflict exists");
        assert!(
            skipped.iter().any(|s| s.contains("moved before")),
            "skipped: {skipped:?}"
        );
    }

    #[test]
    fn unrelated_shorthand_still_swapped() {
        // `value` is shorthand but no explicit field reads it; safe to
        // pull to the front.
        let src = "\
fn make(value: u32, label: String) -> Self {
    Self {
        label: label.clone(),
        value,
    }
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        let value_pos = out.find("value,").expect("value");
        let label_pos = out.find("label: label.clone()").expect("label");
        assert!(value_pos < label_pos, "shorthand first:\n{out}");
    }
}
