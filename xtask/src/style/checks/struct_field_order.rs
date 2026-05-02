//! Fields inside `struct` and `union` should be ordered by `(visibility, type, name)`.
//!
//! Within one block, fields are grouped by visibility (configurable order in
//! `thresholds.toml`); inside each visibility bucket, fields are sorted by
//! type name (last path segment), then by field name.
//!
//! Structs carrying any exempt outer attribute (default: `#[repr(...)]`) are
//! skipped — their layout is part of the contract and must not be reordered.

use std::{cmp::Ordering, ops::Range};

use anyhow::Result;
use syn::{Field, Fields, Item, Type, Visibility, spanned::Spanned};

use super::{Check, Context};
use crate::{
    common::{
        fix::{ExpansionError, FixOutcome, SourceRewriter, expand_blocks},
        parse::parse_file,
        violation::Violation,
        walker::{relative_to, workspace_rs_files_scoped},
    },
    style::config::StructFieldOrderConfig,
};

pub(crate) const ID: &str = "struct_field_order";

pub(crate) struct StructFieldOrder;

impl Check for StructFieldOrder {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.struct_field_order;
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
        let cfg = &ctx.config.thresholds.struct_field_order;
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
    cfg: &StructFieldOrderConfig,
    rel: &str,
    src: &'src str,
    items: &[Item],
    rw: &mut SourceRewriter<'src>,
    skipped: &mut Vec<String>,
) {
    for item in items {
        match item {
            Item::Struct(s) => {
                if has_exempt_attr(&s.attrs, &cfg.exempt_attrs) {
                    continue;
                }
                if let Fields::Named(named) = &s.fields {
                    let collected: Vec<&Field> = named.named.iter().collect();
                    if let Err(reason) = fix_field_block(
                        cfg,
                        src,
                        named.brace_token.span.open().byte_range().end
                            ..named.brace_token.span.close().byte_range().start,
                        &collected,
                        rw,
                    ) {
                        skipped.push(format!(
                            "{rel}:{}: struct `{}`: {reason}",
                            s.ident.span().start().line,
                            s.ident
                        ));
                    }
                }
            }
            Item::Union(u) => {
                if has_exempt_attr(&u.attrs, &cfg.exempt_attrs) {
                    continue;
                }
                let collected: Vec<&Field> = u.fields.named.iter().collect();
                if let Err(reason) = fix_field_block(
                    cfg,
                    src,
                    u.fields.brace_token.span.open().byte_range().end
                        ..u.fields.brace_token.span.close().byte_range().start,
                    &collected,
                    rw,
                ) {
                    skipped.push(format!(
                        "{rel}:{}: union `{}`: {reason}",
                        u.ident.span().start().line,
                        u.ident
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

fn fix_field_block<'src>(
    cfg: &StructFieldOrderConfig,
    src: &'src str,
    scope_bytes: Range<usize>,
    fields: &[&Field],
    rw: &mut SourceRewriter<'src>,
) -> Result<(), String> {
    if fields.len() < 2 {
        return Ok(());
    }

    // If any pair of adjacent fields carries different #[cfg(...)] gates,
    // reordering can change semantics under different feature configs.
    // Skip the whole block.
    if has_heterogeneous_cfg(fields) {
        return Err("fields have heterogeneous `#[cfg(...)]` attributes".to_string());
    }

    let order = build_visibility_order(&cfg.visibility_order);
    let actual: Vec<FieldKey> = fields
        .iter()
        .enumerate()
        .map(|(idx, f)| FieldKey {
            idx,
            vis_bucket: vis_bucket(&order, &f.vis),
            type_key: type_sort_key(&f.ty),
            name: f
                .ident
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
        })
        .collect();
    let mut expected = actual.clone();
    expected.sort_by(cmp_field_key);
    if actual
        .iter()
        .map(|k| k.idx)
        .eq(expected.iter().map(|k| k.idx))
    {
        return Ok(());
    }

    let item_spans: Vec<Range<usize>> = fields.iter().map(|f| f.span().byte_range()).collect();
    let blocks = match expand_blocks(src, scope_bytes, &item_spans) {
        Ok(b) => b,
        Err(ExpansionError::FloatingComment { line, snippet }) => {
            return Err(format!("floating comment at line {line}: `{snippet}`"));
        }
        Err(other) => return Err(format!("engine error: {other:?}")),
    };

    // Same trailing-comma rule as struct_init_order: refuse if the last
    // field has none, since the swap would stitch items together.
    let last = blocks.last().expect("non-empty");
    if !src[last.item_bytes.end..last.bytes.end].contains(',') {
        return Err("last field has no trailing comma".to_string());
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

/// Returns true if any field carries a `#[cfg(...)]` attribute and its
/// neighbour does not, OR any two fields carry different `#[cfg]` text.
/// Conservative: any cfg presence among the fields trips the check.
fn has_heterogeneous_cfg(fields: &[&Field]) -> bool {
    let cfgs: Vec<String> = fields.iter().map(|f| cfg_signature(&f.attrs)).collect();
    cfgs.iter().any(|c| !c.is_empty()) && cfgs.windows(2).any(|w| w[0] != w[1])
}

fn cfg_signature(attrs: &[syn::Attribute]) -> String {
    let mut sigs: Vec<String> = attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg"))
        .map(|a| match &a.meta {
            syn::Meta::List(list) => list.tokens.to_string(),
            other => format!("{other:?}"),
        })
        .collect();
    sigs.sort();
    sigs.join("|")
}

fn scan_items(
    cfg: &StructFieldOrderConfig,
    rel: &str,
    items: &[Item],
    mod_path: &mut Vec<String>,
    out: &mut Vec<Violation>,
) {
    for item in items {
        match item {
            Item::Struct(s) => {
                if has_exempt_attr(&s.attrs, &cfg.exempt_attrs) {
                    continue;
                }
                if let Fields::Named(named) = &s.fields {
                    let collected: Vec<&Field> = named.named.iter().collect();
                    check_field_block(
                        cfg,
                        rel,
                        mod_path,
                        &s.ident.to_string(),
                        "struct",
                        &collected,
                        out,
                    );
                }
            }
            Item::Union(u) => {
                if has_exempt_attr(&u.attrs, &cfg.exempt_attrs) {
                    continue;
                }
                let collected: Vec<&Field> = u.fields.named.iter().collect();
                check_field_block(
                    cfg,
                    rel,
                    mod_path,
                    &u.ident.to_string(),
                    "union",
                    &collected,
                    out,
                );
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

fn check_field_block(
    cfg: &StructFieldOrderConfig,
    rel: &str,
    mod_path: &[String],
    type_name: &str,
    type_kind: &str,
    fields: &[&Field],
    out: &mut Vec<Violation>,
) {
    if fields.len() < 2 {
        return;
    }
    let order = build_visibility_order(&cfg.visibility_order);

    let actual: Vec<FieldKey> = fields
        .iter()
        .enumerate()
        .map(|(idx, f)| FieldKey {
            idx,
            vis_bucket: vis_bucket(&order, &f.vis),
            type_key: type_sort_key(&f.ty),
            name: f
                .ident
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
        })
        .collect();

    let mut expected = actual.clone();
    expected.sort_by(cmp_field_key);

    if actual
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
        "{type_kind} `{type_name}` field order should be (visibility, type, name): \
         expected [{expected_summary}], found [{actual_summary}]"
    );
    out.push(Violation::warn(ID, key, msg));
}

#[derive(Debug, Clone)]
struct FieldKey {
    idx: usize,
    vis_bucket: usize,
    type_key: String,
    name: String,
}

fn cmp_field_key(a: &FieldKey, b: &FieldKey) -> Ordering {
    a.vis_bucket
        .cmp(&b.vis_bucket)
        .then_with(|| a.type_key.cmp(&b.type_key))
        .then_with(|| a.name.cmp(&b.name))
}

/// Map each visibility token in config to its bucket index.
fn build_visibility_order(order: &[String]) -> Vec<&str> {
    order.iter().map(String::as_str).collect()
}

fn vis_bucket(order: &[&str], vis: &Visibility) -> usize {
    let token = vis_token(vis);
    order
        .iter()
        .position(|t| *t == token)
        .unwrap_or(order.len())
}

fn vis_token(vis: &Visibility) -> &'static str {
    match vis {
        Visibility::Public(_) => "pub",
        Visibility::Restricted(r) => {
            if r.path.is_ident("crate") {
                "pub(crate)"
            } else if r.path.is_ident("super") {
                "pub(super)"
            } else {
                "pub(in)"
            }
        }
        Visibility::Inherited => "private",
    }
}

fn has_exempt_attr(attrs: &[syn::Attribute], names: &[String]) -> bool {
    attrs.iter().any(|a| {
        a.path()
            .get_ident()
            .is_some_and(|id| names.iter().any(|n| id == n))
    })
}

/// Stable, last-segment-based sort key for a `Type`.
/// Generics, refs, and pointers are folded into the head ident so
/// `Vec<Foo>` and `Vec<Bar>` group together and then sort by field name.
fn type_sort_key(ty: &Type) -> String {
    match ty {
        Type::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default(),
        Type::Reference(r) => format!("&{}", type_sort_key(&r.elem)),
        Type::Array(a) => format!("[{}]", type_sort_key(&a.elem)),
        Type::Slice(s) => format!("[{}]", type_sort_key(&s.elem)),
        Type::Tuple(t) => {
            let parts = t.elems.iter().map(type_sort_key).collect::<Vec<_>>();
            format!("({})", parts.join(","))
        }
        Type::Ptr(p) => format!("*{}", type_sort_key(&p.elem)),
        Type::Paren(p) => type_sort_key(&p.elem),
        Type::Group(g) => type_sort_key(&g.elem),
        Type::Never(_) => "!".to_string(),
        Type::TraitObject(_) => "dyn".to_string(),
        Type::ImplTrait(_) => "impl".to_string(),
        Type::BareFn(_) => "fn".to_string(),
        Type::Macro(_) => "macro".to_string(),
        Type::Verbatim(_) => "?".to_string(),
        _ => "_".to_string(),
    }
}

#[cfg(test)]
mod fix_tests {
    use std::collections::BTreeMap;

    use super::*;

    fn default_cfg() -> StructFieldOrderConfig {
        StructFieldOrderConfig {
            visibility_order: vec![
                "pub".to_string(),
                "pub(crate)".to_string(),
                "pub(super)".to_string(),
                "pub(in)".to_string(),
                "private".to_string(),
            ],
            exempt_attrs: vec!["repr".to_string()],
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
    fn pub_before_private() {
        let src = "\
struct S {
    private_field: u32,
    pub public_field: u32,
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        let pub_pos = out.find("pub public_field").unwrap();
        let priv_pos = out.find("private_field").unwrap();
        assert!(pub_pos < priv_pos, "pub must precede private:\n{out}");
    }

    #[test]
    fn already_ordered_is_no_op() {
        let src = "\
struct S {
    pub a: u32,
    private: u32,
}
";
        let (out, _) = run_fix(src);
        assert_eq!(out, src);
    }

    #[test]
    fn repr_struct_is_skipped() {
        let src = "\
#[repr(C)]
struct Layout {
    z: u32,
    a: u32,
}
";
        let (out, _) = run_fix(src);
        assert_eq!(out, src, "repr layout must not change");
    }

    #[test]
    fn doc_comments_travel_with_field() {
        let src = "\
struct S {
    /// docs for z
    z: u32,
    /// docs for a
    a: u32,
}
";
        let (out, skipped) = run_fix(src);
        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert_eq!(comment_multiset(src), comment_multiset(&out));
        let a_doc = out.find("/// docs for a").unwrap();
        let a_field = out.find("a: u32,").unwrap();
        let z_doc = out.find("/// docs for z").unwrap();
        let z_field = out.find("z: u32,").unwrap();
        assert!(
            a_doc < a_field && a_field < z_doc && z_doc < z_field,
            "docs must precede their fields:\n{out}"
        );
    }

    #[test]
    fn heterogeneous_cfg_is_skipped() {
        let src = "\
struct S {
    z: u32,
    #[cfg(feature = \"x\")]
    a: u32,
}
";
        let (out, skipped) = run_fix(src);
        assert_eq!(out, src);
        assert!(
            skipped.iter().any(|s| s.contains("cfg")),
            "skipped: {skipped:?}"
        );
    }

    #[test]
    fn missing_trailing_comma_is_skipped() {
        let src = "\
struct S {
    z: u32,
    a: u32
}
";
        let (out, skipped) = run_fix(src);
        assert_eq!(out, src);
        assert!(
            skipped.iter().any(|s| s.contains("trailing comma")),
            "skipped: {skipped:?}"
        );
    }

    #[test]
    fn idempotent_run() {
        let src = "\
struct S {
    z: u32,
    a: u32,
    pub p: u32,
}
";
        let (after_first, _) = run_fix(src);
        let (after_second, _) = run_fix(&after_first);
        assert_eq!(after_first, after_second, "I2: idempotency violated");
    }

    #[test]
    fn floating_comment_skipped() {
        let src = "\
struct S {
    z: u32,

    // floating

    a: u32,
}
";
        let (out, skipped) = run_fix(src);
        assert_eq!(out, src, "must not modify");
        assert!(
            skipped.iter().any(|s| s.contains("floating")),
            "skipped: {skipped:?}"
        );
    }
}
