use anyhow::Result;

use super::{
    Check, Context,
    struct_index::{LiteralSite, WorkspaceStructIndex, build_index, full_literal_sites},
};
use crate::common::{suppress::Suppressions, violation::Violation, walker::compile_globs};

pub(crate) const ID: &str = "field_always_equals_other_field";

pub(crate) struct FieldAlwaysEqualsOtherField;

impl Check for FieldAlwaysEqualsOtherField {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.field_always_equals_other_field;
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
        let full = full_literal_sites(sites);
        if full.len() < min_call_sites {
            continue;
        }
        for (i, a) in info.field_names.iter().enumerate() {
            for b in &info.field_names[i + 1..] {
                let Some(sample) = pair_always_equal(a, b, &full) else {
                    continue;
                };
                let sup = idx.suppressions.get(&info.rel).unwrap_or(&empty);
                if sup.is_suppressed(info.line, ID) {
                    continue;
                }
                let key = format!("{}:{}:{}::{a}=={b}", info.rel, info.line, name);
                out.push(Violation::warn(
                    ID,
                    key,
                    format!(
                        "fields `{name}.{a}` and `{name}.{b}` are initialised with the same \
                         expression (e.g. `{sample}`) at every ({sites}) call site — drop one \
                         of them or expose the shared value through a single accessor",
                        sites = full.len(),
                    ),
                ));
            }
        }
    }
}

/// `Some(sample_expression_string)` if both fields are present and equal at
/// every site, after filtering out the shorthand noise pattern (where `a` is
/// written as `a` and `b` as `b` — the strings match `a` and `b` literally,
/// which doesn't actually mean the values are tied).
fn pair_always_equal(a: &str, b: &str, sites: &[&LiteralSite]) -> Option<String> {
    let mut sample: Option<&str> = None;
    for site in sites {
        let av = site.field_exprs.get(a)?;
        let bv = site.field_exprs.get(b)?;
        if av != bv {
            return None;
        }
        if av == a || av == b {
            return None;
        }
        if sample.is_none() {
            sample = Some(av.as_str());
        }
    }
    sample.map(str::to_string)
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
    fn task_id_equals_seek_epoch_pattern_flagged() {
        let src = r#"
            pub(crate) struct E { task_id: u64, seek_epoch: u64, kind: u32 }
            fn a(epoch: u64) -> E { E { task_id: epoch, seek_epoch: epoch, kind: 1 } }
            fn b(epoch: u64) -> E { E { task_id: epoch, seek_epoch: epoch, kind: 2 } }
            fn c(current_epoch: u64) -> E {
                E { task_id: current_epoch, seek_epoch: current_epoch, kind: 3 }
            }
        "#;
        let ks = keys(src, 3);
        assert_eq!(ks.len(), 1, "{ks:?}");
        assert!(ks[0].contains("task_id==seek_epoch"));
    }

    #[test]
    fn shorthand_a_equals_b_not_flagged() {
        let src = r#"
            pub(crate) struct E { a: u32, b: u32, kind: u32 }
            fn x(a: u32, b: u32) -> E { E { a, b, kind: 1 } }
            fn y(a: u32, b: u32) -> E { E { a, b, kind: 2 } }
            fn z(a: u32, b: u32) -> E { E { a, b, kind: 3 } }
        "#;
        assert_eq!(count(src, 3), 0);
    }

    #[test]
    fn varying_expressions_clean() {
        let src = r#"
            pub(crate) struct E { x: u32, y: u32, kind: u32 }
            fn a(epoch: u64) -> E { E { x: 1, y: 2, kind: 1 } }
            fn b(epoch: u64) -> E { E { x: 3, y: 3, kind: 2 } }
            fn c(epoch: u64) -> E { E { x: 5, y: 5, kind: 3 } }
        "#;
        assert_eq!(count(src, 3), 0);
    }

    #[test]
    fn under_min_skipped() {
        let src = r#"
            pub(crate) struct E { x: u32, y: u32, kind: u32 }
            fn a(e: u64) -> E { E { x: 0, y: 0, kind: 1 } }
            fn b(e: u64) -> E { E { x: 0, y: 0, kind: 2 } }
        "#;
        assert_eq!(count(src, 3), 0);
        assert_eq!(count(src, 2), 1);
    }

    #[test]
    fn rest_spread_excluded() {
        let src = r#"
            pub(crate) struct E { x: u32, y: u32, kind: u32 }
            fn a(e: u32) -> E { E { x: e, y: e, kind: 1 } }
            fn b(e: u32) -> E { E { x: e, y: e, kind: 2 } }
            fn c(base: E) -> E { E { x: 0, y: 0, ..base } }
        "#;
        assert_eq!(count(src, 3), 0);
    }

    #[test]
    fn pub_struct_skipped() {
        let src = r#"
            pub struct E { x: u32, y: u32, kind: u32 }
            fn a(e: u32) -> E { E { x: e, y: e, kind: 1 } }
            fn b(e: u32) -> E { E { x: e, y: e, kind: 2 } }
            fn c(e: u32) -> E { E { x: e, y: e, kind: 3 } }
        "#;
        assert_eq!(count(src, 3), 0);
    }

    #[test]
    fn three_field_chain_flags_each_pair() {
        let src = r#"
            pub(crate) struct E { x: u32, y: u32, z: u32 }
            fn a(e: u32) -> E { E { x: e, y: e, z: e } }
            fn b(e: u32) -> E { E { x: e, y: e, z: e } }
            fn c(e: u32) -> E { E { x: e, y: e, z: e } }
        "#;
        let ks = keys(src, 3);
        assert!(ks.iter().any(|k| k.contains("x==y")), "{ks:?}");
        assert!(ks.iter().any(|k| k.contains("x==z")), "{ks:?}");
        assert!(ks.iter().any(|k| k.contains("y==z")), "{ks:?}");
    }
}
