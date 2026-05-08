//! Function-argument count.
//!
//! Functions with many parameters either: (a) lack a typed value object
//! grouping related inputs, (b) violate single-responsibility (the function
//! is doing several jobs and pulling parameters for each), or (c) chain
//! flag bools that should be a typed enum or builder. Either way, the call
//! site becomes hard to read.
//!
//! Detection counts all signature inputs except `self`. Threshold is
//! intentionally tighter than clippy's `too_many_arguments` default (7),
//! so this check is a *map* of the existing codebase, not an enforced gate.

use anyhow::Result;
use syn::{FnArg, ImplItem, Item, ItemImpl, Signature};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "fn_arg_count";

pub(crate) struct FnArgCount;

impl Check for FnArgCount {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.fn_arg_count;
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");

            let mut hits: Vec<(String, usize)> = Vec::new();
            walk_items(&file.items, &mut Vec::new(), &mut hits);

            for (label, count) in hits {
                if count < cfg.warn {
                    continue;
                }
                let key = format!("{rel}::{label}");
                let msg = format!(
                    "{label}: {count} parameters (warn threshold {}); consider a value \
                     object or splitting responsibilities",
                    cfg.warn
                );
                violations.push(Violation::warn(ID, key, msg));
            }
        }
        Ok(violations)
    }
}

fn walk_items(items: &[Item], scope: &mut Vec<String>, out: &mut Vec<(String, usize)>) {
    for item in items {
        match item {
            Item::Fn(f) => {
                let n = count_args(&f.sig);
                if n > 0 {
                    out.push((qualified(scope, &f.sig.ident.to_string()), n));
                }
            }
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

fn walk_impl(im: &ItemImpl, scope: &[String], out: &mut Vec<(String, usize)>) {
    let owner = self_ty_label(im);
    for it in &im.items {
        if let ImplItem::Fn(f) = it {
            let n = count_args(&f.sig);
            if n > 0 {
                out.push((qualified(scope, &format!("{owner}::{}", f.sig.ident)), n));
            }
        }
    }
}

fn count_args(sig: &Signature) -> usize {
    sig.inputs
        .iter()
        .filter(|i| !matches!(i, FnArg::Receiver(_)))
        .count()
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
