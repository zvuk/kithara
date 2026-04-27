//! Constants should live as close as possible to their usage.
//!
//! For each module-level `const` (with `inherited`/`pub(crate)`/`pub(super)`
//! visibility), count how many `fn` bodies in the same module reference its
//! identifier. Two locality violations are flagged:
//!
//! - **L1 — single-fn**: the const is referenced from exactly one fn. Move
//!   the const inside that fn's body.
//! - **L2 — single-impl**: the const is referenced from ≥2 methods, but all
//!   of those methods belong to the same `impl Foo { ... }` block. Move it
//!   into the impl (accessed as `Self::CONST`).
//!
//! Public (`pub`) constants are skipped: they are part of the cross-crate
//! API and cross-file usage cannot be observed from a single file.

use std::collections::BTreeMap;

use anyhow::Result;
use syn::{Block, Item, ItemImpl, Visibility, visit::Visit};

use super::{Check, Context};
use crate::common::{
    parse::{parse_file, self_ty_name},
    violation::Violation,
    walker::{relative_to, workspace_rs_files},
};

pub(crate) const ID: &str = "const_locality";

pub(crate) struct ConstLocality;

impl Check for ConstLocality {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let mut violations = Vec::new();
        for path in workspace_rs_files(ctx.workspace_root)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            analyze_file(&rel, &file.items, &mut Vec::new(), &mut violations);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

fn analyze_file(rel: &str, items: &[Item], mod_path: &mut Vec<String>, out: &mut Vec<Violation>) {
    let mut consts: Vec<&syn::ItemConst> = Vec::new();
    let mut top_fns: Vec<&syn::ItemFn> = Vec::new();
    let mut impls: Vec<&ItemImpl> = Vec::new();

    for item in items {
        match item {
            Item::Const(c) if is_intra_crate(&c.vis) => consts.push(c),
            Item::Fn(f) => top_fns.push(f),
            Item::Impl(im) => impls.push(im),
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    mod_path.push(m.ident.to_string());
                    analyze_file(rel, inner, mod_path, out);
                    mod_path.pop();
                }
            }
            _ => {}
        }
    }

    if consts.is_empty() {
        return;
    }

    let mod_prefix = if mod_path.is_empty() {
        String::new()
    } else {
        format!("{}::", mod_path.join("::"))
    };

    for c in consts {
        let name = c.ident.to_string();
        let usages = collect_usages(&name, &top_fns, &impls);
        match classify(&usages) {
            Locality::SingleFn(label) => {
                let key = format!("{rel}::{mod_prefix}{name}");
                let msg = format!(
                    "L1: const `{name}` is referenced only from `{label}`; move it \
                     inside that function as a local `const`"
                );
                out.push(Violation::warn(ID, key, msg));
            }
            Locality::SingleImpl(target) => {
                let key = format!("{rel}::{mod_prefix}{name}");
                let msg = format!(
                    "L2: const `{name}` is referenced only by methods of `impl {target}`; \
                     move it into that impl block (accessed as `Self::{name}`)"
                );
                out.push(Violation::warn(ID, key, msg));
            }
            Locality::Spread | Locality::Unused => {}
        }
    }
}

fn is_intra_crate(vis: &Visibility) -> bool {
    match vis {
        Visibility::Inherited => true,
        Visibility::Restricted(r) => r.path.is_ident("crate") || r.path.is_ident("super"),
        Visibility::Public(_) => false,
    }
}

#[derive(Debug)]
enum Owner {
    TopFn(String),
    ImplMethod {
        impl_id: usize,
        target: String,
        method: String,
    },
}

fn collect_usages(name: &str, top_fns: &[&syn::ItemFn], impls: &[&ItemImpl]) -> Vec<Owner> {
    let mut owners = Vec::new();

    for f in top_fns {
        if block_uses_ident(&f.block, name) {
            owners.push(Owner::TopFn(f.sig.ident.to_string()));
        }
    }

    for (idx, im) in impls.iter().enumerate() {
        let target = self_ty_name(&im.self_ty).unwrap_or_else(|| format!("<impl#{idx}>"));
        for it in &im.items {
            if let syn::ImplItem::Fn(method) = it
                && block_uses_ident(&method.block, name)
            {
                owners.push(Owner::ImplMethod {
                    impl_id: idx,
                    target: target.clone(),
                    method: method.sig.ident.to_string(),
                });
            }
        }
    }

    owners
}

#[derive(Debug)]
enum Locality {
    Unused,
    SingleFn(String),
    SingleImpl(String),
    Spread,
}

fn classify(owners: &[Owner]) -> Locality {
    if owners.is_empty() {
        return Locality::Unused;
    }
    if owners.len() == 1 {
        return Locality::SingleFn(format_owner(&owners[0]));
    }
    let mut impl_ids: BTreeMap<usize, &str> = BTreeMap::new();
    let mut top_fn_seen = false;
    for o in owners {
        match o {
            Owner::TopFn(_) => top_fn_seen = true,
            Owner::ImplMethod {
                impl_id, target, ..
            } => {
                impl_ids.insert(*impl_id, target.as_str());
            }
        }
    }
    if !top_fn_seen && impl_ids.len() == 1 {
        let target = impl_ids.values().next().unwrap_or(&"_").to_string();
        return Locality::SingleImpl(target);
    }
    Locality::Spread
}

fn format_owner(o: &Owner) -> String {
    match o {
        Owner::TopFn(name) => format!("fn {name}"),
        Owner::ImplMethod { target, method, .. } => format!("impl {target} :: {method}"),
    }
}

fn block_uses_ident(block: &Block, name: &str) -> bool {
    let mut v = IdentScanner { name, found: false };
    v.visit_block(block);
    v.found
}

struct IdentScanner<'a> {
    name: &'a str,
    found: bool,
}

impl<'ast> Visit<'ast> for IdentScanner<'_> {
    fn visit_path(&mut self, p: &'ast syn::Path) {
        if p.segments.last().is_some_and(|s| s.ident == self.name) {
            self.found = true;
            return;
        }
        syn::visit::visit_path(self, p);
    }
}
