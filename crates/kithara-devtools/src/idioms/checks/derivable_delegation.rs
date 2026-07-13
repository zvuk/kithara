use std::{fs, ops::Range};

use anyhow::{Context as _, Result};
use quote::{ToTokens, quote};
use syn::{Expr, FnArg, ImplItem, ItemImpl, Pat, Stmt, spanned::Spanned};

use super::{Check, Context};
use crate::common::{
    fix::{FixOutcome, SourceRewriter},
    parse::{collect_scopes, self_ty_name},
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
            let Ok(src) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(file) = syn::parse_file(&src) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            for candidate in
                candidates(&src, &file, cfg.trait_min_methods, cfg.inherent_min_methods)
            {
                let detail = candidate.skip.map_or_else(
                    || {
                        format!(
                            "{} impl for {} purely forwards {} methods to self.{}; wrap them in a delegate::delegate! block, or for whole traits used repeatedly consider a trait-delegation derive.",
                            candidate.kind, candidate.target, candidate.methods, candidate.field
                        )
                    },
                    |reason| {
                        format!(
                            "{} impl for {} purely forwards {} methods to self.{} but autofix will skip: {reason}",
                            candidate.kind, candidate.target, candidate.methods, candidate.field
                        )
                    },
                );
                violations.push(Violation::warn(
                    ID,
                    format!("{rel}:{}:0", candidate.line),
                    detail,
                ));
            }
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }

    fn fix(&self, ctx: &Context<'_>) -> Result<FixOutcome> {
        let cfg = &ctx.config.thresholds.derivable_delegation;
        if !cfg.enabled {
            return Ok(FixOutcome::default());
        }
        let mut outcome = FixOutcome::default();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(src) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(file) = syn::parse_file(&src) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            let mut rewriter = SourceRewriter::new(&src);
            for candidate in
                candidates(&src, &file, cfg.trait_min_methods, cfg.inherent_min_methods)
            {
                if let Some(reason) = candidate.skip {
                    outcome.skipped.push(format!(
                        "{rel}:{}: {}: {reason}",
                        candidate.line, candidate.target
                    ));
                    continue;
                }
                rewriter.replace(candidate.range, candidate.replacement);
                outcome.changes.push(format!(
                    "{rel}:{}: delegated {} methods for {} to self.{}",
                    candidate.line, candidate.methods, candidate.target, candidate.field
                ));
            }
            if rewriter.is_empty() {
                continue;
            }
            let rewritten = rewriter.finish().context("apply delegation edits")?;
            fs::write(&path, rewritten).with_context(|| format!("write {}", path.display()))?;
            outcome.writes += 1;
        }
        Ok(outcome)
    }
}

struct Candidate {
    target: String,
    kind: &'static str,
    field: String,
    methods: usize,
    line: usize,
    range: Range<usize>,
    replacement: String,
    skip: Option<&'static str>,
}

fn candidates(
    src: &str,
    file: &syn::File,
    trait_min_methods: usize,
    inherent_min_methods: usize,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    for scope in collect_scopes(file) {
        for impl_block in scope.impls {
            if let Some(candidate) =
                candidate(src, impl_block, trait_min_methods, inherent_min_methods)
            {
                out.push(candidate);
            }
        }
    }
    out
}

fn candidate(
    src: &str,
    impl_block: &ItemImpl,
    trait_min_methods: usize,
    inherent_min_methods: usize,
) -> Option<Candidate> {
    if has_async_trait(&impl_block.attrs) {
        return None;
    }
    let (kind, min_methods) = if impl_block.trait_.is_some() {
        ("trait", trait_min_methods)
    } else {
        ("inherent", inherent_min_methods)
    };
    let methods: Vec<_> = impl_block
        .items
        .iter()
        .filter_map(|item| match item {
            ImplItem::Fn(method) => Some(method),
            _ => None,
        })
        .collect();
    if methods.len() < min_methods {
        return None;
    }
    let field = methods
        .first()
        .and_then(|method| forwarding_field(method))?;
    if !methods
        .iter()
        .all(|method| forwarding_field(method).as_deref() == Some(field.as_str()))
    {
        return None;
    }
    let has_macro = impl_block
        .items
        .iter()
        .any(|item| matches!(item, ImplItem::Macro(_)));
    let has_associated_type = impl_block
        .items
        .iter()
        .any(|item| matches!(item, ImplItem::Type(_)));
    if has_macro || has_associated_type {
        return None;
    }
    let skip = if impl_block.items.len() != methods.len() {
        Some("impl has associated consts or types")
    } else {
        methods
            .iter()
            .find_map(|method| unsupported_method(src, method))
    };
    let first = methods.first()?;
    let last = methods.last()?;
    let start = line_start(src, first.span().byte_range().start);
    let end = line_end(src, last.span().byte_range().end);
    let indent = leading_indent(&src[start..]);
    let inner_indent = format!("{indent}    ");
    let mut replacement =
        format!("{indent}delegate::delegate! {{\n{inner_indent}to self.{field} {{\n");
    for method in &methods {
        for attr in &method.attrs {
            if is_doc_attr(attr) {
                replacement.push_str(&inner_indent);
                replacement.push_str("    ");
                replacement.push_str(src.get(attr.span().byte_range())?);
                replacement.push('\n');
            }
        }
        let visibility = &method.vis;
        let signature = &method.sig;
        let signature = quote!(#visibility #signature).to_string();
        replacement.push_str(&inner_indent);
        replacement.push_str("    ");
        replacement.push_str(&signature);
        replacement.push_str(";\n");
    }
    replacement.push_str(&inner_indent);
    replacement.push_str("}\n");
    replacement.push_str(&indent);
    replacement.push_str("}\n");
    Some(Candidate {
        target: self_ty_name(&impl_block.self_ty).unwrap_or_else(|| "impl".to_owned()),
        kind,
        field,
        methods: methods.len(),
        line: impl_block.impl_token.span.start().line,
        range: start..end,
        replacement,
        skip,
    })
}

fn has_async_trait(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "async_trait")
            || (attr.path().is_ident("cfg_attr")
                && attr
                    .meta
                    .to_token_stream()
                    .to_string()
                    .contains("async_trait"))
    })
}

fn unsupported_method(src: &str, method: &syn::ImplItemFn) -> Option<&'static str> {
    for attr in &method.attrs {
        if !is_doc_attr(attr) {
            return Some("method has a non-doc attribute");
        }
        let range = attr.span().byte_range();
        if src
            .get(range)
            .is_some_and(|text| text.trim_start().starts_with("/**"))
        {
            return Some("block doc-comment cannot be preserved safely");
        }
    }
    None
}

fn is_doc_attr(attr: &syn::Attribute) -> bool {
    attr.path().is_ident("doc")
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

fn line_start(src: &str, at: usize) -> usize {
    src[..at].rfind('\n').map_or(0, |index| index + 1)
}

fn line_end(src: &str, at: usize) -> usize {
    src[at..]
        .find('\n')
        .map_or(src.len(), |index| at + index + 1)
}

fn leading_indent(src: &str) -> String {
    src.chars()
        .take_while(|character| character.is_ascii_whitespace() && *character != '\n')
        .collect()
}

#[cfg(test)]
fn fix_source(source: &str) -> Result<(String, FixOutcome)> {
    let file = syn::parse_file(source)?;
    let mut outcome = FixOutcome::default();
    let mut rewriter = SourceRewriter::new(source);
    for candidate in candidates(source, &file, 2, 3) {
        if let Some(reason) = candidate.skip {
            outcome.skipped.push(reason.to_owned());
            continue;
        }
        outcome.changes.push(candidate.target);
        rewriter.replace(candidate.range, candidate.replacement);
    }
    let changed = !rewriter.is_empty();
    let result = rewriter.finish()?;
    outcome.writes = usize::from(changed);
    Ok((result, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(source: &str) -> usize {
        let file = syn::parse_file(source).expect("valid Rust source");
        candidates(source, &file, 2, 3).len()
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
    fn inherent_impl_is_fixed_and_idempotent() -> Result<()> {
        let source = "impl Wrapper {\n    pub fn read(&self, b: Buf) -> Read { self.inner.read(b) }\n    fn close(&self) { self.inner.close() }\n    fn flush<T>(&self, value: T) -> T { self.inner.flush(value) }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("delegate::delegate!"));
        assert!(fixed.contains("to self.inner"));
        assert!(!fixed.contains("self.inner.read(b)"));
        assert!(!fixed.contains("self.inner.close()"));
        assert!(!fixed.contains("self.inner.flush(value)"));
        let (again, second) = fix_source(&fixed)?;
        assert_eq!(again, fixed);
        assert_eq!(second.writes, 0);
        Ok(())
    }

    #[test]
    fn trait_impl_is_fixed() -> Result<()> {
        let source = "impl Read for Wrapper {\n    fn read(&self, b: Buf) -> Read { self.inner.read(b) }\n    fn close(&self) { self.inner.close() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("delegate::delegate!"));
        assert!(!fixed.contains("self.inner.read(b)"));
        Ok(())
    }

    #[test]
    fn associated_const_is_skipped() -> Result<()> {
        let source = "impl Read for Wrapper { const OPEN: bool = true; fn read(&self, b: Buf) { self.inner.read(b) } fn close(&self) { self.inner.close() } }";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(fixed, source);
        assert_eq!(outcome.skipped, ["impl has associated consts or types"]);
        Ok(())
    }

    #[test]
    fn cfg_method_is_skipped() -> Result<()> {
        let source = "impl Read for Wrapper { #[cfg(unix)] fn read(&self, b: Buf) { self.inner.read(b) } fn close(&self) { self.inner.close() } }";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(fixed, source);
        assert_eq!(outcome.skipped, ["method has a non-doc attribute"]);
        Ok(())
    }

    #[test]
    fn block_doc_comment_is_skipped() -> Result<()> {
        let source = "impl Read for Wrapper { /** Reads. */ fn read(&self, b: Buf) { self.inner.read(b) } fn close(&self) { self.inner.close() } }";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(fixed, source);
        assert_eq!(
            outcome.skipped,
            ["block doc-comment cannot be preserved safely"]
        );
        Ok(())
    }

    #[test]
    fn line_doc_comment_is_preserved() -> Result<()> {
        let source = "impl Read for Wrapper {\n    /// Reads bytes.\n    fn read(&self, b: Buf) { self.inner.read(b) }\n    fn close(&self) { self.inner.close() }\n}\n";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("/// Reads bytes."));
        Ok(())
    }

    #[test]
    fn async_impl_is_fixed() -> Result<()> {
        let source = "impl Read for Wrapper { async fn read(&self, b: Buf) { self.inner.read(b).await } async fn close(&self) { self.inner.close().await } }";
        let (fixed, outcome) = fix_source(source)?;
        assert_eq!(outcome.writes, 1);
        assert!(fixed.contains("async fn read"));
        assert!(!fixed.contains("self.inner.read(b).await"));
        Ok(())
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
    fn cfg_attr_async_trait_impl_is_skipped() {
        assert_eq!(
            count(
                "#[cfg_attr(not(target_arch = \"wasm32\"), async_trait)] #[cfg_attr(target_arch = \"wasm32\", async_trait(?Send))] impl Net for Client { async fn get(&self, r: Req) { self.net.get(r).await } async fn head(&self, r: Req) { self.net.head(r).await } }"
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
