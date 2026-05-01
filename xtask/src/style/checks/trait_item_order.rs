//! Items inside `trait { ... }` and `impl { ... }` should be ordered by
//! `(kind, name)`.
//!
//! Default kind order: `type` → `const` → `fn` → `macro`. Within each kind,
//! items are sorted alphabetically by name. The check applies to traits and
//! both inherent and trait impls by default; configure `apply_to` to narrow.

use std::{cmp::Ordering, ops::Range};

use anyhow::Result;
use syn::{ImplItem, Item, ItemImpl, ItemTrait, TraitItem, spanned::Spanned};

use super::{Check, Context};
use crate::{
    common::{
        fix::{ExpansionError, FixOutcome, SourceRewriter, expand_blocks},
        parse::{parse_file, self_ty_name},
        violation::Violation,
        walker::{relative_to, workspace_rs_files_scoped},
    },
    style::config::TraitItemOrderConfig,
};

pub(crate) const ID: &str = "trait_item_order";

pub(crate) struct TraitItemOrder;

impl Check for TraitItemOrder {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.trait_item_order;
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            scan_items(cfg, &rel, &file.items, &mut Vec::new(), &mut violations);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }

    fn fix(&self, ctx: &Context<'_>) -> Result<FixOutcome> {
        let cfg = &ctx.config.thresholds.trait_item_order;
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
            fix_items(cfg, &rel, &src, &file.items, &mut rw, &mut outcome.skipped);
            if !rw.is_empty() {
                let new_src = rw.finish()?;
                std::fs::write(&path, new_src)?;
                outcome.writes += 1;
            }
        }
        Ok(outcome)
    }
}

fn fix_items<'src>(
    cfg: &TraitItemOrderConfig,
    rel: &str,
    src: &'src str,
    items: &[Item],
    rw: &mut SourceRewriter<'src>,
    skipped: &mut Vec<String>,
) {
    for item in items {
        match item {
            Item::Trait(t) if cfg.apply_to.iter().any(|s| s == "trait") => {
                if let Err(reason) = fix_trait(cfg, src, t, rw) {
                    skipped.push(format!(
                        "{rel}:{}: trait `{}`: {reason}",
                        t.ident.span().start().line,
                        t.ident
                    ));
                }
            }
            Item::Impl(im) => {
                let bucket = if im.trait_.is_some() {
                    "impl_trait"
                } else {
                    "impl_inherent"
                };
                if cfg.apply_to.iter().any(|s| s == bucket)
                    && let Err(reason) = fix_impl(cfg, src, im, rw)
                {
                    let target = self_ty_name(&im.self_ty).unwrap_or_else(|| "<anon>".to_string());
                    skipped.push(format!(
                        "{rel}:{}: impl `{target}`: {reason}",
                        im.impl_token.span.start().line
                    ));
                }
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    fix_items(cfg, rel, src, inner, rw, skipped);
                }
            }
            _ => {}
        }
    }
}

fn fix_trait<'src>(
    cfg: &TraitItemOrderConfig,
    src: &'src str,
    t: &ItemTrait,
    rw: &mut SourceRewriter<'src>,
) -> Result<(), String> {
    if t.items.len() < 2 {
        return Ok(());
    }
    let keys = build_trait_keys(cfg, &t.items);
    let item_spans: Vec<Range<usize>> = t.items.iter().map(|it| it.span().byte_range()).collect();
    let scope_start = t.brace_token.span.open().byte_range().end;
    let scope_end = t.brace_token.span.close().byte_range().start;
    apply_reorder(src, scope_start..scope_end, &item_spans, &keys, rw)
}

fn fix_impl<'src>(
    cfg: &TraitItemOrderConfig,
    src: &'src str,
    im: &ItemImpl,
    rw: &mut SourceRewriter<'src>,
) -> Result<(), String> {
    if im.items.len() < 2 {
        return Ok(());
    }
    let keys = build_impl_keys(cfg, &im.items);
    let item_spans: Vec<Range<usize>> = im.items.iter().map(|it| it.span().byte_range()).collect();
    let scope_start = im.brace_token.span.open().byte_range().end;
    let scope_end = im.brace_token.span.close().byte_range().start;
    apply_reorder(src, scope_start..scope_end, &item_spans, &keys, rw)
}

fn build_trait_keys(cfg: &TraitItemOrderConfig, items: &[TraitItem]) -> Vec<ItemKey> {
    items
        .iter()
        .enumerate()
        .map(|(idx, it)| {
            let (kind, name) = trait_item_kind_and_name(it);
            ItemKey {
                idx,
                kind_bucket: kind_bucket(&cfg.kind_order, kind),
                priority_bucket: priority_bucket(&cfg.priority_fn_names, kind, &name),
                name,
            }
        })
        .collect()
}

fn build_impl_keys(cfg: &TraitItemOrderConfig, items: &[ImplItem]) -> Vec<ItemKey> {
    items
        .iter()
        .enumerate()
        .map(|(idx, it)| {
            let (kind, name) = impl_item_kind_and_name(it);
            ItemKey {
                idx,
                kind_bucket: kind_bucket(&cfg.kind_order, kind),
                priority_bucket: priority_bucket(&cfg.priority_fn_names, kind, &name),
                name,
            }
        })
        .collect()
}

/// Reorder a sequence of sibling items inside a brace-delimited scope.
/// Returns Ok if no swap is needed or after a successful rewrite; Err
/// with a human-readable reason if the engine refuses (floating comment,
/// macro/cfg adjacency).
fn apply_reorder<'src>(
    src: &'src str,
    scope_bytes: Range<usize>,
    item_spans: &[Range<usize>],
    keys: &[ItemKey],
    rw: &mut SourceRewriter<'src>,
) -> Result<(), String> {
    let mut expected = keys.to_vec();
    expected.sort_by(cmp_item_key);
    if keys
        .iter()
        .map(|k| k.idx)
        .eq(expected.iter().map(|k| k.idx))
    {
        return Ok(());
    }
    let blocks = match expand_blocks(src, scope_bytes, item_spans) {
        Ok(b) => b,
        Err(ExpansionError::FloatingComment { line, snippet }) => {
            return Err(format!("floating comment at line {line}: `{snippet}`"));
        }
        Err(other) => return Err(format!("engine error: {other:?}")),
    };
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

fn scan_items(
    cfg: &TraitItemOrderConfig,
    rel: &str,
    items: &[Item],
    mod_path: &mut Vec<String>,
    out: &mut Vec<Violation>,
) {
    for item in items {
        match item {
            Item::Trait(t) if cfg.apply_to.iter().any(|s| s == "trait") => {
                check_trait(cfg, rel, mod_path, t, out);
            }
            Item::Impl(im) => {
                let bucket = if im.trait_.is_some() {
                    "impl_trait"
                } else {
                    "impl_inherent"
                };
                if cfg.apply_to.iter().any(|s| s == bucket) {
                    check_impl(cfg, rel, mod_path, im, bucket, out);
                }
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    mod_path.push(m.ident.to_string());
                    scan_items(cfg, rel, inner, mod_path, out);
                    mod_path.pop();
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
struct ItemKey {
    idx: usize,
    kind_bucket: usize,
    /// Within the `fn` kind, names listed in `priority_fn_names` get a low
    /// bucket index and rank above the alphabetical tail. Other kinds always
    /// share the same priority bucket so only `name` decides their order.
    priority_bucket: usize,
    name: String,
}

fn cmp_item_key(a: &ItemKey, b: &ItemKey) -> Ordering {
    a.kind_bucket
        .cmp(&b.kind_bucket)
        .then_with(|| a.priority_bucket.cmp(&b.priority_bucket))
        .then_with(|| a.name.cmp(&b.name))
}

fn kind_bucket(order: &[String], kind: &str) -> usize {
    order.iter().position(|k| k == kind).unwrap_or(order.len())
}

fn priority_bucket(priority_names: &[String], kind: &str, name: &str) -> usize {
    if kind != "fn" {
        return priority_names.len();
    }
    priority_names
        .iter()
        .position(|n| n == name)
        .unwrap_or(priority_names.len())
}

fn check_trait(
    cfg: &TraitItemOrderConfig,
    rel: &str,
    mod_path: &[String],
    t: &ItemTrait,
    out: &mut Vec<Violation>,
) {
    if t.items.len() < 2 {
        return;
    }
    let keys: Vec<ItemKey> = t
        .items
        .iter()
        .enumerate()
        .map(|(idx, it)| {
            let (kind, name) = trait_item_kind_and_name(it);
            ItemKey {
                idx,
                kind_bucket: kind_bucket(&cfg.kind_order, kind),
                priority_bucket: priority_bucket(&cfg.priority_fn_names, kind, &name),
                name,
            }
        })
        .collect();
    emit_if_misordered(rel, mod_path, &t.ident.to_string(), "trait", &keys, out);
}

fn check_impl(
    cfg: &TraitItemOrderConfig,
    rel: &str,
    mod_path: &[String],
    im: &ItemImpl,
    bucket: &str,
    out: &mut Vec<Violation>,
) {
    if im.items.len() < 2 {
        return;
    }
    let keys: Vec<ItemKey> = im
        .items
        .iter()
        .enumerate()
        .map(|(idx, it)| {
            let (kind, name) = impl_item_kind_and_name(it);
            ItemKey {
                idx,
                kind_bucket: kind_bucket(&cfg.kind_order, kind),
                priority_bucket: priority_bucket(&cfg.priority_fn_names, kind, &name),
                name,
            }
        })
        .collect();
    let target = self_ty_name(&im.self_ty).unwrap_or_else(|| "<anon>".to_string());
    let label = match (bucket, &im.trait_) {
        ("impl_trait", Some((_, p, _))) => {
            let trait_name = p
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default();
            format!("{trait_name} for {target}")
        }
        _ => target,
    };
    emit_if_misordered(rel, mod_path, &label, bucket, &keys, out);
}

fn emit_if_misordered(
    rel: &str,
    mod_path: &[String],
    type_name: &str,
    block_kind: &str,
    keys: &[ItemKey],
    out: &mut Vec<Violation>,
) {
    let mut expected = keys.to_vec();
    expected.sort_by(cmp_item_key);
    if keys
        .iter()
        .map(|k| k.idx)
        .eq(expected.iter().map(|k| k.idx))
    {
        return;
    }
    let mod_prefix = if mod_path.is_empty() {
        String::new()
    } else {
        format!("{}::", mod_path.join("::"))
    };
    let key = format!("{rel}::{mod_prefix}{type_name}");
    let actual_summary = keys
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
        "{block_kind} `{type_name}` items should be ordered (kind, name): \
         expected [{expected_summary}], found [{actual_summary}]"
    );
    out.push(Violation::warn(ID, key, msg));
}

fn trait_item_kind_and_name(it: &TraitItem) -> (&'static str, String) {
    match it {
        TraitItem::Type(t) => ("type", t.ident.to_string()),
        TraitItem::Const(c) => ("const", c.ident.to_string()),
        TraitItem::Fn(f) => ("fn", f.sig.ident.to_string()),
        TraitItem::Macro(m) => (
            "macro",
            m.mac
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default(),
        ),
        _ => ("other", String::new()),
    }
}

fn impl_item_kind_and_name(it: &ImplItem) -> (&'static str, String) {
    match it {
        ImplItem::Type(t) => ("type", t.ident.to_string()),
        ImplItem::Const(c) => ("const", c.ident.to_string()),
        ImplItem::Fn(f) => ("fn", f.sig.ident.to_string()),
        ImplItem::Macro(m) => (
            "macro",
            m.mac
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default(),
        ),
        _ => ("other", String::new()),
    }
}

#[cfg(test)]
mod fix_tests {
    use std::collections::BTreeMap;

    use super::*;

    fn default_cfg() -> TraitItemOrderConfig {
        TraitItemOrderConfig {
            apply_to: vec![
                "trait".to_string(),
                "impl_inherent".to_string(),
                "impl_trait".to_string(),
            ],
            kind_order: vec![
                "type".to_string(),
                "const".to_string(),
                "fn".to_string(),
                "macro".to_string(),
            ],
            priority_fn_names: vec!["new".to_string()],
        }
    }

    fn run_fix(src: &str) -> (String, Vec<String>) {
        let cfg = default_cfg();
        let file = syn::parse_file(src).unwrap_or_else(|e| panic!("parse failed: {e}\n---\n{src}"));
        let mut rw = SourceRewriter::new(src);
        let mut skipped = Vec::new();
        fix_items(&cfg, "fixture.rs", src, &file.items, &mut rw, &mut skipped);
        let out = if rw.is_empty() {
            src.to_string()
        } else {
            rw.finish().expect("rewriter finish")
        };
        (out, skipped)
    }

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
    fn impl_methods_alphabetised() {
        let src = "\
struct Foo;

impl Foo {
    fn zebra(&self) {}
    fn alpha(&self) {}
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        let alpha_pos = out.find("fn alpha").unwrap();
        let zebra_pos = out.find("fn zebra").unwrap();
        assert!(alpha_pos < zebra_pos, "alpha should precede zebra:\n{out}");
    }

    #[test]
    fn type_then_const_then_fn() {
        let src = "\
trait Foo {
    fn one();
    const C: u32;
    type Item;
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        let type_pos = out.find("type Item").unwrap();
        let const_pos = out.find("const C").unwrap();
        let fn_pos = out.find("fn one").unwrap();
        assert!(
            type_pos < const_pos && const_pos < fn_pos,
            "kind order:\n{out}"
        );
    }

    #[test]
    fn already_ordered_is_no_op() {
        let src = "\
struct Foo;

impl Foo {
    fn alpha(&self) {}
    fn zebra(&self) {}
}
";
        let (out, _) = run_fix(src);
        assert_eq!(out, src);
    }

    #[test]
    fn doc_comments_travel_with_method() {
        let src = "\
struct Foo;

impl Foo {
    /// docs for zebra
    fn zebra(&self) {}

    /// docs for alpha
    fn alpha(&self) {}
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        let alpha_doc = out.find("/// docs for alpha").unwrap();
        let alpha_fn = out.find("fn alpha").unwrap();
        let zebra_doc = out.find("/// docs for zebra").unwrap();
        let zebra_fn = out.find("fn zebra").unwrap();
        assert!(
            alpha_doc < alpha_fn && alpha_fn < zebra_doc && zebra_doc < zebra_fn,
            "docs must precede their fn:\n{out}"
        );
    }

    #[test]
    fn outer_comments_attached_no_blank_travel() {
        let src = "\
trait Foo {
    // inline note for zebra
    fn zebra();
    // inline note for alpha
    fn alpha();
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert_eq!(comment_multiset(src), comment_multiset(&out));
        // The comment for alpha must end up above `fn alpha`, BEFORE the
        // zebra comment.
        let alpha_note = out.find("// inline note for alpha").unwrap();
        let alpha_fn = out.find("fn alpha").unwrap();
        let zebra_note = out.find("// inline note for zebra").unwrap();
        assert!(
            alpha_note < alpha_fn && alpha_fn < zebra_note,
            "comments did not travel:\n{out}"
        );
    }

    #[test]
    fn idempotent_run() {
        let src = "\
trait Foo {
    fn zebra();
    fn alpha();
}
";
        let (after_first, _) = run_fix(src);
        let (after_second, _) = run_fix(&after_first);
        assert_eq!(after_first, after_second, "I2: idempotency violated");
    }

    #[test]
    fn priority_fn_name_keeps_new_first() {
        let src = "\
struct Foo;

impl Foo {
    fn alpha(&self) {}
    fn new() -> Self { Foo }
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        let new_pos = out.find("fn new").unwrap();
        let alpha_pos = out.find("fn alpha").unwrap();
        assert!(new_pos < alpha_pos, "priority fn `new` first:\n{out}");
    }
}
