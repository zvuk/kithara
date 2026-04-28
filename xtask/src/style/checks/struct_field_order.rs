//! Fields inside `struct` and `union` should be ordered by `(visibility, type, name)`.
//!
//! Within one block, fields are grouped by visibility (configurable order in
//! `thresholds.toml`); inside each visibility bucket, fields are sorted by
//! type name (last path segment), then by field name.
//!
//! Structs carrying any exempt outer attribute (default: `#[repr(...)]`) are
//! skipped — their layout is part of the contract and must not be reordered.

use std::cmp::Ordering;

use anyhow::Result;
use syn::{Field, Fields, Item, Type, Visibility};

use super::{Check, Context};
use crate::{
    common::{
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
