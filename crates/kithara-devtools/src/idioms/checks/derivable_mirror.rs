use std::fs;

use anyhow::Result;
use syn::{Expr, ImplItem, ItemImpl, Member, Pat, Path, Stmt, visit, visit::Visit};

use super::{Check, Context};
use crate::{
    common::{parse::self_ty_name, violation::Violation, walker::relative_to},
    idioms::config::DerivableSeverity,
};

pub(crate) struct DerivableMirror;

impl Check for DerivableMirror {
    fn id(&self) -> &'static str {
        "derivable_mirror"
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let config = &ctx.config.thresholds.derivable_mirror;
        if !config.enabled {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for path in ctx.scan.rs_files(ctx.scope)?.iter() {
            let source = fs::read_to_string(path)?;
            let relative = relative_to(ctx.workspace_root, path).to_string_lossy();
            for (name, line) in check_source(&source) {
                let key = format!("{relative}:{line}:0");
                let message = format!(
                    "complete structural conversion for {name}: use #[derive(kithara_derive::Mirror)]"
                );
                out.push(match config.severity {
                    DerivableSeverity::Deny => Violation::deny(self.id(), key, message),
                    DerivableSeverity::Warn => Violation::warn(self.id(), key, message),
                });
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }
}

fn check_source(source: &str) -> Vec<(String, usize)> {
    let Ok(file) = syn::parse_file(source) else {
        return Vec::new();
    };
    let mut visitor = MirrorVisitor::default();
    visitor.visit_file(&file);
    visitor.findings
}

#[derive(Default)]
struct MirrorVisitor {
    findings: Vec<(String, usize)>,
}

impl<'ast> Visit<'ast> for MirrorVisitor {
    fn visit_item_impl(&mut self, implementation: &'ast ItemImpl) {
        let is_from = implementation
            .trait_
            .as_ref()
            .and_then(|(path, _)| path.segments.last())
            .is_some_and(|segment| segment.ident == "From");
        if is_from
            && matches!(implementation.items.as_slice(), [ImplItem::Fn(method)] if method.sig.ident == "from" && structural_body(&method.block.stmts))
            && let Some(name) = self_ty_name(&implementation.self_ty)
        {
            self.findings
                .push((name, implementation.impl_token.span.start().line));
        }
        visit::visit_item_impl(self, implementation);
    }
}

fn structural_body(statements: &[Stmt]) -> bool {
    let [Stmt::Expr(expression, None)] = statements else {
        return false;
    };
    match expression {
        Expr::Struct(value) => value.rest.is_none() && value.fields.iter().all(same_named_field),
        Expr::Match(value) => {
            matches!(&*value.expr, Expr::Path(_))
                && !value.arms.is_empty()
                && value.arms.iter().all(structural_arm)
        }
        _ => false,
    }
}

fn structural_arm(arm: &syn::Arm) -> bool {
    let Some((source_variant, source_fields)) = pattern_shape(&arm.pat) else {
        return false;
    };
    let Some((target_variant, target_fields)) = constructor_shape(&arm.body) else {
        return false;
    };
    source_variant == target_variant && source_fields == target_fields
}

fn pattern_shape(pattern: &Pat) -> Option<(String, Vec<String>)> {
    match pattern {
        Pat::Path(value) => Some((last(&value.path)?.to_string(), Vec::new())),
        Pat::TupleStruct(value) => Some((
            last(&value.path)?.to_string(),
            value
                .elems
                .iter()
                .map(pattern_ident)
                .collect::<Option<Vec<_>>>()?,
        )),
        Pat::Struct(value) if value.rest.is_none() => Some((
            last(&value.path)?.to_string(),
            value
                .fields
                .iter()
                .map(|field| {
                    let member = member_name(&field.member)?;
                    (pattern_ident(&field.pat)? == member).then_some(member)
                })
                .collect::<Option<Vec<_>>>()?,
        )),
        Pat::Reference(value) => pattern_shape(&value.pat),
        _ => None,
    }
}

fn constructor_shape(expression: &Expr) -> Option<(String, Vec<String>)> {
    match expression {
        Expr::Path(value) => Some((last(&value.path)?.to_string(), Vec::new())),
        Expr::Call(value) => {
            let Expr::Path(path) = &*value.func else {
                return None;
            };
            Some((
                last(&path.path)?.to_string(),
                value
                    .args
                    .iter()
                    .map(expression_ident)
                    .collect::<Option<Vec<_>>>()?,
            ))
        }
        Expr::Struct(value) if value.rest.is_none() => Some((
            last(&value.path)?.to_string(),
            value
                .fields
                .iter()
                .map(|field| {
                    let member = member_name(&field.member)?;
                    (expression_ident(&field.expr)? == member).then_some(member)
                })
                .collect::<Option<Vec<_>>>()?,
        )),
        Expr::Block(value) => {
            let [Stmt::Expr(expression, None)] = value.block.stmts.as_slice() else {
                return None;
            };
            constructor_shape(expression)
        }
        _ => None,
    }
}

fn same_named_field(field: &syn::FieldValue) -> bool {
    let Some(target) = member_name(&field.member) else {
        return false;
    };
    let Expr::Field(source) = &field.expr else {
        return false;
    };
    member_name(&source.member).is_some_and(|source| source == target)
        && matches!(&*source.base, Expr::Path(_))
}

fn pattern_ident(pattern: &Pat) -> Option<String> {
    let Pat::Ident(value) = pattern else {
        return None;
    };
    (value.subpat.is_none()).then(|| value.ident.to_string())
}

fn expression_ident(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Path(value) => last(&value.path).map(ToString::to_string),
        Expr::Unary(value) => expression_ident(&value.expr),
        Expr::Reference(value) => expression_ident(&value.expr),
        Expr::Paren(value) => expression_ident(&value.expr),
        _ => None,
    }
}

fn member_name(member: &Member) -> Option<String> {
    match member {
        Member::Named(ident) => Some(ident.to_string()),
        Member::Unnamed(_) => None,
    }
}

fn last(path: &Path) -> Option<&syn::Ident> {
    path.segments.last().map(|segment| &segment.ident)
}

#[cfg(test)]
mod tests {
    use super::check_source;

    #[test]
    fn reports_structs_and_complete_enum_matches() {
        let source = r#"
impl From<Source> for Target { fn from(value: Source) -> Self { Self { first: value.first, kept: value.kept } } }
impl From<SourceKind> for TargetKind { fn from(value: SourceKind) -> Self { match value { SourceKind::A => Self::A, SourceKind::B(v) => Self::B(v) } } }
"#;
        assert_eq!(check_source(source).len(), 2);
    }

    #[test]
    fn ignores_policy_and_partial_conversions() {
        let source = r#"
impl From<Source> for Target { fn from(value: Source) -> Self { Self { value: value.compute() } } }
impl From<SourceKind> for TargetKind { fn from(value: SourceKind) -> Self { match value { SourceKind::A => Self::A, _ => Self::Unknown } } }
impl From<Policy> for Output { fn from(value: Policy) -> Self { match value { Policy::Enabled => Self::On, Policy::Disabled => Self::Off } } }
"#;
        assert!(check_source(source).is_empty());
    }
    #[test]
    fn ignores_empty_matches_for_uninhabited_sources() {
        let source = r#"
enum Empty {}
enum Target { Ready }
impl From<Empty> for Target { fn from(value: Empty) -> Self { match value {} } }
impl From<core::convert::Infallible> for Output { fn from(value: core::convert::Infallible) -> Self { match value {} } }
"#;
        assert!(check_source(source).is_empty());
    }
}
