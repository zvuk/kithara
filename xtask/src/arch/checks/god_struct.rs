//! `fields + methods` per struct.
//!
//! Catches god-types: a struct with many fields *and* many methods is doing
//! too much. Methods include both inherent (`impl X`) and trait-impl
//! (`impl Trait for X`) functions, aggregated across every `impl` block in
//! the file (so a type whose surface is split across many traits still hits
//! the threshold). Fields are counted from the struct definition.
//!
//! Tuple structs and unit structs count as zero fields. Generic params do
//! not count as fields.

use std::collections::BTreeMap;

use anyhow::Result;
use syn::{Fields, ImplItem, Item, ItemImpl, ItemStruct, Type};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "god_struct";

pub(crate) struct GodStruct;

impl Check for GodStruct {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.god_struct;
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");

            let mut structs: BTreeMap<String, usize> = BTreeMap::new();
            let mut impl_fns: BTreeMap<String, usize> = BTreeMap::new();
            collect(&file.items, &mut structs, &mut impl_fns);

            for (name, fields) in &structs {
                let fns = impl_fns.get(name).copied().unwrap_or(0);
                let total = fields + fns;
                if total >= cfg.warn {
                    let key = format!("{rel}::{name}");
                    let msg = format!(
                        "{name}: {fields} fields + {fns} methods = {total} (warn threshold {})",
                        cfg.warn
                    );
                    violations.push(Violation::warn(ID, key, msg));
                }
            }
        }
        Ok(violations)
    }
}

fn collect(
    items: &[Item],
    structs: &mut BTreeMap<String, usize>,
    impl_fns: &mut BTreeMap<String, usize>,
) {
    for item in items {
        match item {
            Item::Struct(s) => {
                structs.insert(s.ident.to_string(), count_fields(s));
            }
            Item::Impl(im) => {
                if let Some(name) = self_ty_name(im) {
                    let fns = im
                        .items
                        .iter()
                        .filter(|it| matches!(it, ImplItem::Fn(_)))
                        .count();
                    *impl_fns.entry(name).or_insert(0) += fns;
                }
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect(inner, structs, impl_fns);
                }
            }
            _ => {}
        }
    }
}

fn count_fields(s: &ItemStruct) -> usize {
    match &s.fields {
        Fields::Named(n) => n.named.len(),
        Fields::Unnamed(u) => u.unnamed.len(),
        Fields::Unit => 0,
    }
}

fn self_ty_name(im: &ItemImpl) -> Option<String> {
    match im.self_ty.as_ref() {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}
