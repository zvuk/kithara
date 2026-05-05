//! Flag `impl T` blocks that expose more than one non-`self` constructor.
//!
//! The user policy is one canonical constructor per type (`new` or
//! `default`); alternate construction shapes go through `Config`/`Options`
//! parameter structs. Multiple `fn x() -> Self` / `fn y() -> Self`
//! methods inside one inherent `impl` are flagged.

use std::collections::HashSet;

use anyhow::Result;
use syn::{ImplItem, Item, ItemImpl, ReturnType, Type};

use super::{Check, Context};
use crate::{
    arch::config::MultiConstructorThreshold,
    common::{
        parse::{parse_file, self_ty_name},
        suppress::Suppressions,
        violation::Violation,
        walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
    },
};

pub(crate) const ID: &str = "multi_constructor";

pub(crate) struct MultiConstructor;

impl Check for MultiConstructor {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.multi_constructor;
        let exempt = compile_globs(&cfg.exempt_files);
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path);
            if matches_any(&exempt, rel) {
                continue;
            }
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let src = std::fs::read_to_string(&path)?;
            let suppress = Suppressions::parse(&src);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            analyze_file(&rel_str, &file.items, cfg, &suppress, &mut violations);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

fn analyze_file(
    rel: &str,
    items: &[Item],
    cfg: &MultiConstructorThreshold,
    suppress: &Suppressions,
    out: &mut Vec<Violation>,
) {
    for item in items {
        match item {
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    analyze_file(rel, inner, cfg, suppress, out);
                }
            }
            Item::Impl(im) if im.trait_.is_none() => {
                inspect_impl(rel, im, cfg, suppress, out);
            }
            _ => {}
        }
    }
}

fn inspect_impl(
    rel: &str,
    im: &ItemImpl,
    cfg: &MultiConstructorThreshold,
    suppress: &Suppressions,
    out: &mut Vec<Violation>,
) {
    let target = self_ty_name(&im.self_ty).unwrap_or_else(|| "<impl>".into());
    let mut ctors: Vec<String> = Vec::new();
    for it in &im.items {
        let ImplItem::Fn(f) = it else { continue };
        if has_self_receiver(f) {
            continue;
        }
        if !returns_self(&f.sig.output, &target) {
            continue;
        }
        ctors.push(f.sig.ident.to_string());
    }
    if ctors.len() <= 1 {
        return;
    }
    let allowed: HashSet<&str> = cfg.canonical_names.iter().map(String::as_str).collect();
    if !ctors.iter().any(|n| !allowed.contains(n.as_str())) {
        return;
    }
    let line = im.impl_token.span.start().line;
    if suppress.is_suppressed(line, ID) {
        return;
    }
    let key = format!("{rel}:{line}:{target}");
    out.push(Violation::warn(
        ID,
        key,
        format!(
            "`impl {target}` exposes {} constructors ({}); keep one canonical \
             (`new`/`default`) and pass alternative shapes through a `Config`/`Options` struct",
            ctors.len(),
            ctors.join(", "),
        ),
    ));
}

fn has_self_receiver(f: &syn::ImplItemFn) -> bool {
    f.sig
        .inputs
        .first()
        .is_some_and(|a| matches!(a, syn::FnArg::Receiver(_)))
}

fn returns_self(out: &ReturnType, target: &str) -> bool {
    let ReturnType::Type(_, ty) = out else {
        return false;
    };
    matches_self_or(ty, target)
}

fn matches_self_or(ty: &Type, target: &str) -> bool {
    match ty {
        Type::Path(tp) => {
            tp.path
                .segments
                .last()
                .is_some_and(|s| s.ident == "Self" || s.ident == target)
                || is_result_of_self(tp, target)
        }
        _ => false,
    }
}

fn is_result_of_self(tp: &syn::TypePath, target: &str) -> bool {
    let Some(seg) = tp.path.segments.last() else {
        return false;
    };
    if seg.ident != "Result" {
        return false;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return false;
    };
    args.args.iter().any(|a| match a {
        syn::GenericArgument::Type(t) => matches_self_or(t, target),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::suppress::Suppressions;

    fn count(src: &str) -> usize {
        let cfg = MultiConstructorThreshold::default();
        let suppress = Suppressions::parse(src);
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let mut out = Vec::new();
        analyze_file("fixture.rs", &file.items, &cfg, &suppress, &mut out);
        out.len()
    }

    #[test]
    fn single_new_ok() {
        assert_eq!(count("struct S; impl S { fn new() -> Self { Self } }"), 0);
    }

    #[test]
    fn new_and_with_capacity_flagged() {
        let src = "struct S; impl S { fn new() -> Self { Self } \
                   fn with_capacity(_: usize) -> Self { Self } }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn new_and_default_ok() {
        let src = "struct S; impl S { fn new() -> Self { Self } \
                   fn default() -> Self { Self } }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn three_ctors_flagged_once_per_impl() {
        let src = "struct S; impl S { \
                   fn new() -> Self { Self } \
                   fn from_str(_: &str) -> Self { Self } \
                   fn create_with(_: u8) -> Self { Self } }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn trait_impl_excluded() {
        let src = "trait T { fn make() -> Self; fn dup() -> Self; } \
                   struct S; impl T for S { fn make() -> Self { S } fn dup() -> Self { S } }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn methods_with_self_excluded() {
        let src = "struct S; impl S { fn new() -> Self { Self } \
                   fn clone(&self) -> Self { S } }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn result_self_counts() {
        let src = "struct S; type Err = (); \
                   impl S { fn new() -> Result<Self, Err> { Ok(S) } \
                            fn open(_: u32) -> Result<Self, Err> { Ok(S) } }";
        assert_eq!(count(src), 1);
    }
}
