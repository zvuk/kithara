//! Items inside `trait { ... }` and `impl { ... }` should be ordered by
//! `(kind, name)`.
//!
//! Default kind order: `type` → `const` → `fn` → `macro`. Within each kind,
//! items are sorted alphabetically by name. The check applies to traits and
//! both inherent and trait impls by default; configure `apply_to` to narrow.

use std::cmp::Ordering;

use anyhow::Result;
use syn::{ImplItem, Item, ItemImpl, ItemTrait, TraitItem};

use super::{Check, Context};
use crate::{
    common::{
        parse::{parse_file, self_ty_name},
        violation::Violation,
        walker::{relative_to, workspace_rs_files},
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
        for path in workspace_rs_files(ctx.workspace_root)? {
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
    name: String,
}

fn cmp_item_key(a: &ItemKey, b: &ItemKey) -> Ordering {
    a.kind_bucket
        .cmp(&b.kind_bucket)
        .then_with(|| a.name.cmp(&b.name))
}

fn kind_bucket(order: &[String], kind: &str) -> usize {
    order.iter().position(|k| k == kind).unwrap_or(order.len())
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
