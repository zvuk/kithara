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
        return Err("contains `..base` rest".to_string());
    }

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
        return Ok(());
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

    let last_block_text = &src[blocks.last().expect("non-empty").bytes.clone()];
    if !last_block_text.trim_end().ends_with(',')
        && !last_block_text.trim_end().ends_with([')', '}', ']'])
    {
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
    a.bucket.cmp(&b.bucket).then_with(|| a.idx.cmp(&b.idx))
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
    // Mirror the autofix safety model: a `..base` rest or a shorthand field
    // read by an earlier explicit initializer makes reordering to
    // declaration order a move-before-read (use-after-move) — the fix
    // refuses these, so detection must not flag them either.
    if e.dot2_token.is_some() || e.rest.is_some() {
        return;
    }
    if has_shorthand_use_def_conflict(&e.fields) {
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
        assert_eq!(comment_multiset(src), comment_multiset(&out));
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
        assert_eq!(comment_multiset(src), comment_multiset(&out));
        let comment_pos = out.find("// glued to y").expect("comment");
        let x_pos = out.find("x: 1,").expect("x: 1,");
        assert!(comment_pos < x_pos, "comment did not travel with y:\n{out}");
    }

    #[test]
    fn missing_trailing_comma_is_skipped() {
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
        let src = "fn main() { let _ = Foo { x: 1, y, z: 2, }; }";
        let (after_first, skipped1) = run_fix(src);
        assert!(skipped1.is_empty());
        let (after_second, skipped2) = run_fix(&after_first);
        assert_eq!(after_first, after_second, "I2: idempotency violated");
        assert!(skipped2.is_empty());
    }

    #[test]
    fn shorthand_use_def_conflict_is_skipped() {
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

#[cfg(test)]
mod detect_tests {
    use syn::visit::Visit;

    use super::*;

    fn default_cfg() -> StructInitOrderConfig {
        StructInitOrderConfig {
            shorthand_first: true,
        }
    }

    /// Run the detection visitor over a snippet and return the keys of the
    /// violations it reports.
    fn detect(src: &str) -> Vec<String> {
        let cfg = default_cfg();
        let file = syn::parse_file(src).unwrap_or_else(|e| panic!("parse failed: {e}\n---\n{src}"));
        let mut out = Vec::new();
        let mut v = InitVisitor {
            cfg: &cfg,
            rel: "fixture.rs",
            out: &mut out,
        };
        v.visit_file(&file);
        out.into_iter().map(|viol| viol.key).collect()
    }

    #[test]
    fn out_of_order_explicit_is_flagged() {
        let src = "fn main() { let _ = Foo { x: 1, y, }; }";
        assert_eq!(
            detect(src).len(),
            1,
            "genuinely reorderable literal must fire"
        );
    }

    #[test]
    fn shorthand_use_def_conflict_is_not_flagged() {
        let src = "\
fn make(pcm: Pcm) -> Self {
    Self {
        spec: pcm.spec(),
        pcm,
    }
}
";
        assert!(
            detect(src).is_empty(),
            "reordering would move `pcm` before `spec` reads it — must not flag"
        );
    }

    #[test]
    fn rest_base_is_not_flagged() {
        let src = "fn main() { let base = Foo::default(); let _ = Foo { x: 1, y, ..base }; }";
        assert!(
            detect(src).is_empty(),
            "`..base` literals are skipped by the fix and must not be flagged"
        );
    }

    #[test]
    fn ui_state_use_def_conflict_is_not_flagged() {
        let src = "\
fn build(ui_state: UiState, controller: Controller) -> Self {
    Self {
        controller,
        previous_volume: ui_state.volume.max(0.01),
        ui_state,
    }
}
";
        assert!(
            detect(src).is_empty(),
            "shorthand `ui_state` is read by an earlier explicit field — must not flag"
        );
    }

    #[test]
    fn unrelated_shorthand_is_still_flagged() {
        let src = "\
fn make(value: u32, label: String) -> Self {
    Self {
        label: label.clone(),
        value,
    }
}
";
        assert_eq!(
            detect(src).len(),
            1,
            "no use-def conflict: `value` is not read by `label`'s init — must still flag"
        );
    }
}
