//! Flag wrapper-structs that exist solely to bypass `clippy::too_many_arguments`.
//!
//! Anti-pattern shape:
//!
//! ```ignore
//! pub(crate) struct BuildPair<'a> {
//!     pub(crate) backend: AssetStore<DecryptContext>,
//!     pub(crate) config: &'a HlsConfig,
//!     // ... 6 more fields
//! }
//!
//! pub(crate) fn build_pair(args: BuildPair<'_>) -> (HlsScheduler, HlsSource) {
//!     let BuildPair { backend, config, /* ... */ } = args;
//!     // ...
//! }
//! ```
//!
//! The struct exists only to be passed to `build_pair`, destructured in the
//! first statement, and discarded. It dodges clippy's argument-count rule
//! without giving the call sites real cohesion. The fix is one of: split the
//! function, promote the struct to a real domain type used in ≥2 places, or
//! invest in a builder.
//!
//! Detection (workspace-wide):
//!
//! 1. The struct has named fields, declares ≥`min_fields`, and is *not*
//!    bare `pub` (cross-crate API can be reconstructed externally — we'd be
//!    blind to those call sites).
//! 2. The struct has no inherent `impl X { fn ... }` methods. `#[derive(...)]`
//!    and hand-written `impl Trait for X` (e.g. `Default`) don't count.
//! 3. There are ≥`min_call_sites` literal sites for `X { ... }`, and *every*
//!    one of them is a direct argument to the same function.
//! 4. That function destructures `X` in its first statement
//!    (`let X { .. } = arg;`).

use anyhow::Result;

use super::{
    Check, Context,
    struct_index::{WorkspaceStructIndex, build_index, unique_consumer},
};
use crate::common::{suppress::Suppressions, violation::Violation, walker::compile_globs};

pub(crate) const ID: &str = "args_wrapper_struct";

pub(crate) struct ArgsWrapperStruct;

impl Check for ArgsWrapperStruct {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.args_wrapper_struct;
        let exempt = compile_globs(&cfg.exempt_files);
        let idx = build_index(ctx.workspace_root, ctx.scope, &exempt)?;
        let mut out = Vec::new();
        emit(&idx, cfg.min_fields, cfg.min_call_sites, &mut out);
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }
}

fn emit(
    idx: &WorkspaceStructIndex,
    min_fields: usize,
    min_call_sites: usize,
    out: &mut Vec<Violation>,
) {
    let empty = Suppressions::default();
    for (name, info) in &idx.structs {
        if info.is_pub {
            continue;
        }
        if info.field_names.len() < min_fields {
            continue;
        }
        if idx.impl_method_counts.get(name).copied().unwrap_or(0) > 0 {
            continue;
        }
        let Some(sites) = idx.literals.get(name) else {
            continue;
        };
        if sites.len() < min_call_sites {
            continue;
        }
        let Some(consumer) = unique_consumer(sites) else {
            continue;
        };
        let consumes_via_destructure = idx
            .destructuring_consumers
            .get(name)
            .is_some_and(|set| set.iter().any(|fn_name| fn_name == consumer));
        if !consumes_via_destructure {
            continue;
        }
        let sup = idx.suppressions.get(&info.rel).unwrap_or(&empty);
        if sup.is_suppressed(info.line, ID) {
            continue;
        }
        let key = format!("{}:{}:{}", info.rel, info.line, name);
        out.push(Violation::warn(
            ID,
            key,
            format!(
                "`struct {name}` ({} fields) has no methods and is only built to be passed to \
                 `{consumer}`, which destructures it on the first line; either split `{consumer}` \
                 into smaller calls, promote `{name}` to a real domain type used by ≥2 \
                 functions, or replace it with a proper builder",
                info.field_names.len(),
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{arch::checks::struct_index::build_index_from_source, common::scope::Scope};

    fn count(src: &str) -> usize {
        let idx = build_index_from_source(src);
        let mut out = Vec::new();
        emit(&idx, 5, 2, &mut out);
        out.len()
    }

    fn count_with(src: &str, min_fields: usize, min_call_sites: usize) -> usize {
        let idx = build_index_from_source(src);
        let mut out = Vec::new();
        emit(&idx, min_fields, min_call_sites, &mut out);
        out.len()
    }

    #[test]
    fn classic_args_wrapper_flagged() {
        let src = r#"
            pub(crate) struct BuildPair {
                a: u32, b: u32, c: u32, d: u32, e: u32,
            }
            pub(crate) fn build_pair(args: BuildPair) -> u32 {
                let BuildPair { a, b, c, d, e } = args;
                a + b + c + d + e
            }
            fn first() -> u32 { build_pair(BuildPair { a: 1, b: 2, c: 3, d: 4, e: 5 }) }
            fn second() -> u32 { build_pair(BuildPair { a: 9, b: 9, c: 9, d: 9, e: 9 }) }
        "#;
        assert_eq!(count(src), 1);
        let _ = Scope::default();
    }

    #[test]
    fn struct_with_inherent_methods_clean() {
        let src = r#"
            pub(crate) struct Cfg { a: u32, b: u32, c: u32, d: u32, e: u32 }
            impl Cfg { pub fn new() -> Self { Self { a:0, b:0, c:0, d:0, e:0 } } }
            pub(crate) fn use_cfg(args: Cfg) -> u32 {
                let Cfg { a, b, c, d, e } = args;
                a + b + c + d + e
            }
            fn first() -> u32 { use_cfg(Cfg { a:1, b:2, c:3, d:4, e:5 }) }
            fn second() -> u32 { use_cfg(Cfg { a:9, b:9, c:9, d:9, e:9 }) }
        "#;
        assert_eq!(count(src), 0);
    }

    #[test]
    fn pub_struct_skipped() {
        let src = r#"
            pub struct External { a: u32, b: u32, c: u32, d: u32, e: u32 }
            pub(crate) fn f(args: External) -> u32 {
                let External { a, b, c, d, e } = args;
                a + b + c + d + e
            }
            fn first() -> u32 { f(External { a:1, b:2, c:3, d:4, e:5 }) }
            fn second() -> u32 { f(External { a:9, b:9, c:9, d:9, e:9 }) }
        "#;
        assert_eq!(count(src), 0);
    }

    #[test]
    fn under_field_threshold_skipped() {
        let src = r#"
            pub(crate) struct Tiny { a: u32, b: u32, c: u32, d: u32 }
            pub(crate) fn f(args: Tiny) -> u32 {
                let Tiny { a, b, c, d } = args;
                a + b + c + d
            }
            fn first() -> u32 { f(Tiny { a:1, b:2, c:3, d:4 }) }
            fn second() -> u32 { f(Tiny { a:9, b:9, c:9, d:9 }) }
        "#;
        assert_eq!(count(src), 0);
    }

    #[test]
    fn used_in_two_different_functions_clean() {
        let src = r#"
            pub(crate) struct Real { a: u32, b: u32, c: u32, d: u32, e: u32 }
            pub(crate) fn f(args: Real) -> u32 {
                let Real { a, b, c, d, e } = args;
                a + b + c + d + e
            }
            pub(crate) fn g(args: Real) -> u32 {
                let Real { a, b, c, d, e } = args;
                a * b * c * d * e
            }
            fn caller_f() -> u32 { f(Real { a:1, b:2, c:3, d:4, e:5 }) }
            fn caller_g() -> u32 { g(Real { a:9, b:9, c:9, d:9, e:9 }) }
        "#;
        assert_eq!(count(src), 0);
    }

    #[test]
    fn used_outside_call_argument_clean() {
        let src = r#"
            pub(crate) struct Ctx { a: u32, b: u32, c: u32, d: u32, e: u32 }
            pub(crate) fn consume(args: Ctx) -> u32 {
                let Ctx { a, b, c, d, e } = args;
                a + b + c + d + e
            }
            fn caller() -> u32 {
                let saved = Ctx { a:1, b:2, c:3, d:4, e:5 };
                consume(Ctx { a:9, b:9, c:9, d:9, e:9 }) + saved.a
            }
        "#;
        assert_eq!(count(src), 0);
    }

    #[test]
    fn reference_argument_position_flagged() {
        let src = r#"
            pub(crate) struct Ctx { a: u32, b: u32, c: u32, d: u32, e: u32 }
            pub(crate) fn consume(ctx: &Ctx) -> u32 {
                let &Ctx { a, b, c, d, e } = ctx;
                a + b + c + d + e
            }
            fn first() -> u32 { consume(&Ctx { a:1, b:2, c:3, d:4, e:5 }) }
            fn second() -> u32 { consume(&Ctx { a:9, b:9, c:9, d:9, e:9 }) }
        "#;
        assert_eq!(count(src), 1);
    }

    #[test]
    fn single_call_site_skipped_at_default_threshold() {
        let src = r#"
            pub(crate) struct One { a: u32, b: u32, c: u32, d: u32, e: u32 }
            pub(crate) fn consume(args: One) -> u32 {
                let One { a, b, c, d, e } = args;
                a + b + c + d + e
            }
            fn caller() -> u32 { consume(One { a:1, b:2, c:3, d:4, e:5 }) }
        "#;
        assert_eq!(count(src), 0);
        assert_eq!(count_with(src, 5, 1), 1);
    }
}
