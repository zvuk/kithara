//! Fields in `Foo { ... }` literal expressions should follow:
//!
//! 1. Shorthand fields (`field_a` without `: value`) come first.
//! 2. Explicit fields (`field_b: value`) come after, alphabetically by name.
//! 3. The optional `..base` rest comes last (Rust syntax already enforces this).
//!
//! Tuple-struct expressions (`Foo(a, b)`) and explicit-position struct exprs
//! (`Foo { 0: a, 1: b }`) are skipped — they have no name to sort by.

use std::cmp::Ordering;

use anyhow::Result;
use proc_macro2::Span;
use syn::{ExprStruct, FieldValue, Member, visit::Visit};

use super::{Check, Context};
use crate::{
    common::{
        parse::parse_file,
        violation::Violation,
        walker::{relative_to, workspace_rs_files},
    },
    style::config::StructInitOrderConfig,
};

pub(crate) const ID: &str = "struct_init_order";

pub(crate) struct StructInitOrder;

impl Check for StructInitOrder {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.struct_init_order;
        let mut violations = Vec::new();
        for path in workspace_rs_files(ctx.workspace_root)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");

            let mut v = InitVisitor {
                cfg,
                rel: &rel,
                out: &mut violations,
            };
            v.visit_file(&file);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

struct InitVisitor<'a> {
    cfg: &'a StructInitOrderConfig,
    rel: &'a str,
    out: &'a mut Vec<Violation>,
}

impl<'ast> Visit<'ast> for InitVisitor<'_> {
    fn visit_expr_struct(&mut self, e: &'ast ExprStruct) {
        check_expr_struct(self.cfg, self.rel, e, self.out);
        syn::visit::visit_expr_struct(self, e);
    }
}

#[derive(Debug, Clone)]
struct InitKey {
    idx: usize,
    /// 0 = shorthand, 1 = explicit, 2 = unnamed/positional (sorts last).
    bucket: usize,
    name: String,
}

fn cmp_init_key(a: &InitKey, b: &InitKey) -> Ordering {
    a.bucket.cmp(&b.bucket).then_with(|| a.name.cmp(&b.name))
}

fn classify(cfg: &StructInitOrderConfig, fv: &FieldValue) -> InitKey {
    let is_shorthand = fv.colon_token.is_none();
    let bucket = match &fv.member {
        Member::Unnamed(_) => 2,
        Member::Named(_) if is_shorthand && cfg.shorthand_first => 0,
        Member::Named(_) => 1,
    };
    let name = match &fv.member {
        Member::Named(id) => id.to_string(),
        Member::Unnamed(i) => i.index.to_string(),
    };
    InitKey {
        idx: 0,
        bucket,
        name,
    }
}

fn check_expr_struct(
    cfg: &StructInitOrderConfig,
    rel: &str,
    e: &ExprStruct,
    out: &mut Vec<Violation>,
) {
    if e.fields.len() < 2 {
        return;
    }
    let actual: Vec<InitKey> = e
        .fields
        .iter()
        .enumerate()
        .map(|(idx, fv)| InitKey {
            idx,
            ..classify(cfg, fv)
        })
        .collect();

    let mut expected = actual.clone();
    expected.sort_by(cmp_init_key);

    if actual
        .iter()
        .map(|k| k.idx)
        .eq(expected.iter().map(|k| k.idx))
    {
        return;
    }

    let type_name = e
        .path
        .segments
        .last()
        .map_or_else(|| "<anon>".to_string(), |s| s.ident.to_string());
    let line = type_span_start_line(e);
    let key = format!("{rel}:{line}::{type_name}");
    let actual_summary = actual
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
        "init `{type_name} {{ ... }}` should be (shorthand first, then explicit by name): \
         expected [{expected_summary}], found [{actual_summary}]"
    );
    out.push(Violation::warn(ID, key, msg));
}

fn type_span_start_line(e: &ExprStruct) -> usize {
    let span = e
        .path
        .segments
        .first()
        .map_or_else(Span::call_site, syn::spanned::Spanned::span);
    span.start().line
}
