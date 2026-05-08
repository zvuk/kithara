//! Generic-parameter and where-clause bound counts.
//!
//! Functions/structs/traits with many generic parameters or many
//! where-clause predicates almost always indicate over-abstraction:
//! the type is trying to be too general for the few callers it has.
//! High generic load is usually a smell of "design for hypothetical
//! future requirements" — pick the concrete shape the code needs now.
//!
//! Detection counts:
//!   * generic params: type-params + lifetime-params + const-params
//!     (the `<T, 'a, N: usize>` count, declared on a function/struct/trait);
//!   * where bounds: number of where-clause predicates (each `T: Trait + …`
//!     entry is one predicate, regardless of trait-bound count inside).
//!
//! Two thresholds in one check; either one triggers a separate violation
//! line so the report lists which bound was breached.

use anyhow::Result;
use syn::{Generics, ImplItem, Item, ItemImpl};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "generic_param_count";

pub(crate) struct GenericParamCount;

impl Check for GenericParamCount {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.generic_param_count;
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");

            let mut hits: Vec<Hit> = Vec::new();
            walk_items(&file.items, &mut Vec::new(), &mut hits);

            for h in hits {
                let key = format!("{rel}::{label}", label = h.label);
                if h.params >= cfg.warn_params {
                    violations.push(Violation::warn(
                        ID,
                        key.clone(),
                        format!(
                            "{l}: {n} generic parameters (warn threshold {th}); narrow the \
                             abstraction or pick a concrete shape",
                            l = h.label,
                            n = h.params,
                            th = cfg.warn_params
                        ),
                    ));
                }
                if h.where_bounds >= cfg.warn_where {
                    violations.push(Violation::warn(
                        ID,
                        key,
                        format!(
                            "{l}: {n} where-clause predicates (warn threshold {th}); \
                             collapse into a sealed/narrower trait",
                            l = h.label,
                            n = h.where_bounds,
                            th = cfg.warn_where
                        ),
                    ));
                }
            }
        }
        Ok(violations)
    }
}

struct Hit {
    label: String,
    params: usize,
    where_bounds: usize,
}

fn walk_items(items: &[Item], scope: &mut Vec<String>, out: &mut Vec<Hit>) {
    for item in items {
        match item {
            Item::Fn(f) => push(out, scope, &f.sig.ident.to_string(), &f.sig.generics),
            Item::Struct(s) => push(out, scope, &s.ident.to_string(), &s.generics),
            Item::Enum(e) => push(out, scope, &e.ident.to_string(), &e.generics),
            Item::Trait(t) => push(out, scope, &t.ident.to_string(), &t.generics),
            Item::Type(t) => push(out, scope, &t.ident.to_string(), &t.generics),
            Item::Impl(im) => walk_impl(im, scope, out),
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    scope.push(m.ident.to_string());
                    walk_items(inner, scope, out);
                    scope.pop();
                }
            }
            _ => {}
        }
    }
}

fn walk_impl(im: &ItemImpl, scope: &[String], out: &mut Vec<Hit>) {
    let owner = self_ty_label(im);
    for it in &im.items {
        if let ImplItem::Fn(f) = it {
            push(
                out,
                scope,
                &format!("{owner}::{}", f.sig.ident),
                &f.sig.generics,
            );
        }
    }
}

fn push(out: &mut Vec<Hit>, scope: &[String], name: &str, g: &Generics) {
    let params = g.params.len();
    let where_bounds = g.where_clause.as_ref().map_or(0, |w| w.predicates.len());
    if params == 0 && where_bounds == 0 {
        return;
    }
    out.push(Hit {
        label: qualified(scope, name),
        params,
        where_bounds,
    });
}

fn qualified(scope: &[String], name: &str) -> String {
    if scope.is_empty() {
        name.to_string()
    } else {
        format!("{}::{name}", scope.join("::"))
    }
}

fn self_ty_label(im: &ItemImpl) -> String {
    match im.self_ty.as_ref() {
        syn::Type::Path(p) => p
            .path
            .segments
            .last()
            .map_or_else(|| "?".to_string(), |s| s.ident.to_string()),
        _ => "?".to_string(),
    }
}
