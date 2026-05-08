//! Number of `impl Trait for X` blocks targeting one local type.
//!
//! Many distinct trait impls on a single type signal a god-type playing
//! several roles. The companion shape is "one type, one role", with role
//! composition done via a wrapper or an embedded struct rather than by
//! piling more `impl Trait` blocks on the same type.
//!
//! Detection is per-file: only types defined in the current file are
//! considered, and only `impl Trait for X` (not inherent `impl X`) blocks
//! count. Cross-file impls of foreign types belong elsewhere (extension-
//! trait pattern) and are out of scope here.

use std::collections::BTreeMap;

use anyhow::Result;
use syn::{Item, ItemImpl, Type};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "trait_impl_count";

pub(crate) struct TraitImplCount;

impl Check for TraitImplCount {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.trait_impl_count;
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");

            let mut local_types: BTreeMap<String, usize> = BTreeMap::new();
            collect_local_types(&file.items, &mut local_types);
            count_trait_impls(&file.items, &mut local_types);

            for (name, n) in &local_types {
                if *n >= cfg.warn {
                    let key = format!("{rel}::{name}");
                    let msg = format!(
                        "{name}: {n} trait impls in this file (warn threshold {}); \
                         consider splitting roles via wrapper/embedded types",
                        cfg.warn
                    );
                    violations.push(Violation::warn(ID, key, msg));
                }
            }
        }
        Ok(violations)
    }
}

fn collect_local_types(items: &[Item], out: &mut BTreeMap<String, usize>) {
    for item in items {
        match item {
            Item::Struct(s) => {
                out.entry(s.ident.to_string()).or_insert(0);
            }
            Item::Enum(e) => {
                out.entry(e.ident.to_string()).or_insert(0);
            }
            Item::Type(t) => {
                out.entry(t.ident.to_string()).or_insert(0);
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect_local_types(inner, out);
                }
            }
            _ => {}
        }
    }
}

fn count_trait_impls(items: &[Item], local_types: &mut BTreeMap<String, usize>) {
    for item in items {
        match item {
            Item::Impl(im) if is_trait_impl(im) => {
                if let Some(name) = self_ty_name(im)
                    && let Some(c) = local_types.get_mut(&name)
                {
                    *c += 1;
                }
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    count_trait_impls(inner, local_types);
                }
            }
            _ => {}
        }
    }
}

fn is_trait_impl(im: &ItemImpl) -> bool {
    im.trait_.is_some()
}

fn self_ty_name(im: &ItemImpl) -> Option<String> {
    match im.self_ty.as_ref() {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}
