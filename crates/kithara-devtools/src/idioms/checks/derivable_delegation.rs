use anyhow::Result;
use syn::{Expr, FnArg, ImplItem, ItemImpl, Pat, Stmt};

use super::{Check, Context};
use crate::common::{
    parse::{collect_scopes, parse_file, self_ty_name},
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "derivable_delegation";

pub(crate) struct DerivableDelegation;

impl Check for DerivableDelegation {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.derivable_delegation;
        if !cfg.enabled {
            return Ok(Vec::new());
        }
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            for scope in collect_scopes(&file) {
                for impl_block in scope.impls {
                    check_impl(
                        impl_block,
                        &rel,
                        cfg.trait_min_methods,
                        cfg.inherent_min_methods,
                        &mut violations,
                    );
                }
            }
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

fn check_impl(
    impl_block: &ItemImpl,
    rel: &str,
    trait_min_methods: usize,
    inherent_min_methods: usize,
    out: &mut Vec<Violation>,
) {
    // `delegate!` cannot expand inside an `#[async_trait]` impl (E0195
    // lifetime mismatch against the desugared signatures), so such impls
    // have no macro collapse path and must not be flagged.
    if impl_block.attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|seg| seg.ident == "async_trait")
    }) {
        return;
    }
    let min_methods = if impl_block.trait_.is_some() {
        trait_min_methods
    } else {
        inherent_min_methods
    };
    let methods: Vec<_> = impl_block
        .items
        .iter()
        .filter_map(|item| match item {
            ImplItem::Fn(method) => Some(method),
            _ => None,
        })
        .collect();
    if methods.len() < min_methods || methods.len() != impl_block.items.len() {
        return;
    }
    let Some(field) = methods.first().and_then(|method| forwarding_field(method)) else {
        return;
    };
    if !methods
        .iter()
        .all(|method| forwarding_field(method).as_deref() == Some(field.as_str()))
    {
        return;
    }
    let pos = impl_block.impl_token.span.start();
    let target = self_ty_name(&impl_block.self_ty).unwrap_or_else(|| "impl".to_owned());
    let kind = if impl_block.trait_.is_some() {
        "trait"
    } else {
        "inherent"
    };
    out.push(Violation::warn(
        ID,
        format!("{rel}:{}:{}", pos.line, pos.column),
        format!(
            "{kind} impl for {target} purely forwards {} methods to self.{field}; wrap them in a delegate::delegate! block, or for whole traits used repeatedly consider a trait-delegation derive.",
            methods.len()
        ),
    ));
}

fn forwarding_field(method: &syn::ImplItemFn) -> Option<String> {
    let [Stmt::Expr(expr, None)] = method.block.stmts.as_slice() else {
        return None;
    };
    let expr = match expr {
        Expr::MethodCall(_) if method.sig.asyncness.is_none() => expr,
        Expr::Await(await_expr) if method.sig.asyncness.is_some() => &await_expr.base,
        _ => return None,
    };
    let Expr::MethodCall(call) = expr else {
        return None;
    };
    if call.method != method.sig.ident || call.turbofish.is_some() {
        return None;
    }
    let Expr::Field(receiver) = &*call.receiver else {
        return None;
    };
    if !matches!(&*receiver.base, Expr::Path(path) if path.path.is_ident("self")) {
        return None;
    }
    let syn::Member::Named(field) = &receiver.member else {
        return None;
    };
    let params: Vec<_> = method
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Receiver(_) => None,
            FnArg::Typed(arg) => match &*arg.pat {
                Pat::Ident(ident) if ident.subpat.is_none() => Some(ident.ident.to_string()),
                _ => Some(String::new()),
            },
        })
        .collect();
    if params.len() != call.args.len()
        || call
            .args
            .iter()
            .zip(params)
            .any(|(arg, param)| !matches!(arg, Expr::Path(path) if path.path.is_ident(&param)))
    {
        return None;
    }
    Some(field.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(source: &str) -> usize {
        let file = syn::parse_file(source).expect("valid Rust source");
        let mut out = Vec::new();
        for item in file.items {
            if let syn::Item::Impl(impl_block) = item {
                check_impl(&impl_block, "fixture.rs", 2, 3, &mut out);
            }
        }
        out.len()
    }

    #[test]
    fn trait_impl_is_flagged() {
        assert_eq!(
            count(
                "impl Read for Wrapper { fn read(&self, b: Buf) { self.inner.read(b) } fn close(&self) { self.inner.close() } }"
            ),
            1
        );
    }

    #[test]
    fn inherent_impl_is_flagged() {
        assert_eq!(
            count(
                "impl Wrapper { fn read(&self, b: Buf) { self.inner.read(b) } fn close(&self) { self.inner.close() } fn flush(&self) { self.inner.flush() } }"
            ),
            1
        );
    }

    #[test]
    fn transformed_argument_is_rejected() {
        assert_eq!(
            count(
                "impl Read for Wrapper { fn read(&self, b: Buf) { self.inner.read(b.into()) } fn close(&self) { self.inner.close() } }"
            ),
            0
        );
    }

    #[test]
    fn extra_statement_is_rejected() {
        assert_eq!(
            count(
                "impl Read for Wrapper { fn read(&self, b: Buf) { trace(); self.inner.read(b) } fn close(&self) { self.inner.close() } }"
            ),
            0
        );
    }

    #[test]
    fn different_fields_are_rejected() {
        assert_eq!(
            count(
                "impl Read for Wrapper { fn read(&self, b: Buf) { self.inner.read(b) } fn close(&self) { self.other.close() } }"
            ),
            0
        );
    }

    #[test]
    fn async_trait_attribute_impl_is_skipped() {
        assert_eq!(
            count(
                "#[async_trait::async_trait] impl Net for Client { async fn get(&self, r: Req) { self.net.get(r).await } async fn head(&self, r: Req) { self.net.head(r).await } }"
            ),
            0
        );
    }

    #[test]
    fn async_trait_impl_is_flagged() {
        assert_eq!(
            count(
                "impl Read for Wrapper { async fn read(&self, b: Buf) { self.inner.read(b).await } async fn close(&self) { self.inner.close().await } }"
            ),
            1
        );
    }

    #[test]
    fn transformed_async_argument_is_rejected() {
        assert_eq!(
            count(
                "impl Read for Wrapper { async fn read(&self, b: Buf) { self.inner.read(b.into()).await } async fn close(&self) { self.inner.close().await } }"
            ),
            0
        );
    }

    #[test]
    fn associated_type_is_rejected() {
        assert_eq!(
            count(
                "impl Read for Wrapper { type Error = Error; fn read(&self, b: Buf) { self.inner.read(b) } fn close(&self) { self.inner.close() } }"
            ),
            0
        );
    }
}
