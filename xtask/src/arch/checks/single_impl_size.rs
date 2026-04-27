//! Per-`impl`-block LOC threshold.
//!
//! Catches the case `file_density` misses: a single `impl Trait for X` block
//! that spans hundreds of lines while the file overall has a healthy `fn`-
//! to-`type` ratio. Example: a 459-line `impl Source for FileSource` does not
//! trigger `file_density` (10 fns / 1 type = 10:1, well below 25:1) yet is
//! clearly a candidate to split by responsibility.

use anyhow::Result;
use syn::{Item, ItemImpl, Type, spanned::Spanned};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    violation::Violation,
    walker::{relative_to, workspace_rs_files},
};

pub(crate) const ID: &str = "single_impl_size";

pub(crate) struct SingleImplSize;

impl Check for SingleImplSize {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.single_impl_size;
        let mut violations = Vec::new();

        for path in workspace_rs_files(ctx.workspace_root)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");

            let mut impls = Vec::new();
            collect_impls(&file.items, &mut impls);

            for im in impls {
                let span = im.span();
                let start = span.start().line;
                let end = span.end().line;
                if start == 0 || end < start {
                    continue; // span info unavailable — skip silently
                }
                let lines = end - start + 1;
                let label = describe_impl(im);
                let key = format!("{rel}::{label}");
                let msg = if lines >= cfg.deny_lines {
                    format!(
                        "{label} spans {lines} lines (deny threshold {}); split by \
                         responsibility (e.g. read/write/seek into separate impl blocks \
                         or extension traits)",
                        cfg.deny_lines
                    )
                } else if lines >= cfg.warn_lines {
                    format!(
                        "{label} spans {lines} lines (warn threshold {}); consider \
                         splitting by responsibility",
                        cfg.warn_lines
                    )
                } else {
                    continue;
                };
                if lines >= cfg.deny_lines {
                    violations.push(Violation::deny(ID, key, msg));
                } else {
                    violations.push(Violation::warn(ID, key, msg));
                }
            }
        }
        Ok(violations)
    }
}

fn collect_impls<'a>(items: &'a [Item], out: &mut Vec<&'a ItemImpl>) {
    for item in items {
        match item {
            Item::Impl(im) => out.push(im),
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect_impls(inner, out);
                }
            }
            _ => {}
        }
    }
}

fn describe_impl(im: &ItemImpl) -> String {
    let target = type_label(&im.self_ty);
    if let Some((_, path, _)) = &im.trait_ {
        let trait_name = path
            .segments
            .last()
            .map_or_else(|| "?".to_string(), |s| s.ident.to_string());
        format!("impl {trait_name} for {target}")
    } else {
        format!("impl {target}")
    }
}

fn type_label(ty: &Type) -> String {
    match ty {
        Type::Path(p) => p
            .path
            .segments
            .last()
            .map_or_else(|| "?".to_string(), |s| s.ident.to_string()),
        _ => "?".to_string(),
    }
}
