//! Flag struct fields that are initialised with the same expression at
//! *every* call site. The field is effectively a constant — code reading the
//! struct elsewhere keeps re-deriving a value the construction sites all
//! agreed on. Either inline the value, drop the field, or redesign so the
//! field actually varies.
//!
//! Detection (workspace-wide):
//!
//! - Look at every struct with named fields.
//! - Skip fields that don't appear in every fully-specified literal site
//!   (sites with `..base` are ignored — we can't see their values).
//! - For each field, collect normalised token strings of the assigned
//!   expression at every site. If they're all identical and there are
//!   ≥`min_call_sites` sites, flag.
//! - `pub` (cross-crate) structs are skipped — external callers may build
//!   them in ways the index can't observe.

use anyhow::Result;

use super::{
    Check, Context,
    struct_index::{LiteralSite, WorkspaceStructIndex, build_index, full_literal_sites},
};
use crate::common::{suppress::Suppressions, violation::Violation, walker::compile_globs};

pub(crate) const ID: &str = "field_always_constant";

pub(crate) struct FieldAlwaysConstant;

impl Check for FieldAlwaysConstant {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.field_always_constant;
        let exempt = compile_globs(&cfg.exempt_files);
        let idx = build_index(ctx.workspace_root, ctx.scope, &exempt)?;
        let mut out = Vec::new();
        emit(&idx, cfg.min_call_sites, &mut out);
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }
}

fn emit(idx: &WorkspaceStructIndex, min_call_sites: usize, out: &mut Vec<Violation>) {
    let empty = Suppressions::default();
    for (name, info) in &idx.structs {
        if info.is_pub {
            continue;
        }
        let Some(sites) = idx.literals.get(name) else {
            continue;
        };
        let full_sites = full_literal_sites(sites);
        if full_sites.len() < min_call_sites {
            continue;
        }
        for field in &info.field_names {
            let Some(value) = constant_value(field, &full_sites) else {
                continue;
            };
            let sup = idx.suppressions.get(&info.rel).unwrap_or(&empty);
            if sup.is_suppressed(info.line, ID) {
                continue;
            }
            let key = format!("{}:{}:{}::{}", info.rel, info.line, name, field);
            out.push(Violation::warn(
                ID,
                key,
                format!(
                    "field `{name}.{field}` is initialised with `{value}` at every \
                     ({sites}) call site — fold it into a constant or method, or \
                     drop the field if it's redundant",
                    sites = full_sites.len(),
                ),
            ));
        }
    }
}

/// Return the shared expression string when *every* site assigns the same
/// expression to `field`. `None` if expressions differ, the field is missing
/// from any site, or there are no sites.
fn constant_value(field: &str, sites: &[&LiteralSite]) -> Option<String> {
    let mut shared: Option<&str> = None;
    for site in sites {
        let value = site.field_exprs.get(field)?;
        match shared {
            None => shared = Some(value.as_str()),
            Some(prev) if prev == value.as_str() => {}
            Some(_) => return None,
        }
    }
    shared.map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::checks::struct_index::build_index_from_source;

    fn count(src: &str, min: usize) -> usize {
        let idx = build_index_from_source(src);
        let mut out = Vec::new();
        emit(&idx, min, &mut out);
        out.len()
    }

    fn keys(src: &str, min: usize) -> Vec<String> {
        let idx = build_index_from_source(src);
        let mut out = Vec::new();
        emit(&idx, min, &mut out);
        out.into_iter().map(|v| v.key).collect()
    }

    #[test]
    fn field_always_zero_flagged() {
        let src = r#"
            pub(crate) struct Ev { id: u32, kind: u32 }
            fn a() -> Ev { Ev { id: 0, kind: 1 } }
            fn b() -> Ev { Ev { id: 0, kind: 2 } }
            fn c() -> Ev { Ev { id: 0, kind: 3 } }
        "#;
        assert_eq!(count(src, 3), 1);
    }

    #[test]
    fn task_id_equals_seek_epoch_pattern_flagged_per_field() {
        // Both `task_id` and `seek_epoch` are written as `epoch` in every
        // site — each is independently flagged as a constant (=`epoch`).
        // The cross-field equality is `field_always_equals_other_field`'s
        // job, not this check's.
        let src = r#"
            pub(crate) struct E { task_id: u64, seek_epoch: u64, kind: u32 }
            fn a(epoch: u64) -> E { E { task_id: epoch, seek_epoch: epoch, kind: 1 } }
            fn b(epoch: u64) -> E { E { task_id: epoch, seek_epoch: epoch, kind: 2 } }
            fn c(epoch: u64) -> E { E { task_id: epoch, seek_epoch: epoch, kind: 3 } }
        "#;
        let ks = keys(src, 3);
        assert!(ks.iter().any(|k| k.ends_with("E::task_id")), "{ks:?}");
        assert!(ks.iter().any(|k| k.ends_with("E::seek_epoch")), "{ks:?}");
    }

    #[test]
    fn varying_field_clean() {
        let src = r#"
            pub(crate) struct E { id: u32, kind: u32 }
            fn a() -> E { E { id: 0, kind: 1 } }
            fn b() -> E { E { id: 1, kind: 2 } }
            fn c() -> E { E { id: 2, kind: 3 } }
        "#;
        assert_eq!(count(src, 3), 0);
    }

    #[test]
    fn under_min_call_sites_skipped() {
        let src = r#"
            pub(crate) struct E { id: u32, kind: u32 }
            fn a() -> E { E { id: 0, kind: 1 } }
            fn b() -> E { E { id: 0, kind: 2 } }
        "#;
        assert_eq!(count(src, 3), 0);
        assert_eq!(count(src, 2), 1);
    }

    #[test]
    fn rest_spread_sites_excluded() {
        // The third site spreads `..base`, so `id` value is unknown there.
        // The remaining two are below the default threshold — clean.
        let src = r#"
            pub(crate) struct E { id: u32, kind: u32 }
            fn a() -> E { E { id: 0, kind: 1 } }
            fn b() -> E { E { id: 0, kind: 2 } }
            fn c(base: E) -> E { E { id: 0, ..base } }
        "#;
        assert_eq!(count(src, 3), 0);
    }

    #[test]
    fn pub_struct_skipped() {
        let src = r#"
            pub struct E { id: u32, kind: u32 }
            fn a() -> E { E { id: 0, kind: 1 } }
            fn b() -> E { E { id: 0, kind: 2 } }
            fn c() -> E { E { id: 0, kind: 3 } }
        "#;
        assert_eq!(count(src, 3), 0);
    }

    #[test]
    fn shorthand_counts_as_same_expression() {
        // `X { a }` is sugar for `X { a: a }`. If the same variable name is
        // used at every site (intentional or accidental), expressions match.
        let src = r#"
            pub(crate) struct E { id: u32, kind: u32 }
            fn a() -> E { let id = 0; E { id, kind: 1 } }
            fn b() -> E { let id = 0; E { id, kind: 2 } }
            fn c() -> E { let id = 0; E { id, kind: 3 } }
        "#;
        let ks = keys(src, 3);
        assert!(ks.iter().any(|k| k.ends_with("E::id")), "{ks:?}");
    }
}
