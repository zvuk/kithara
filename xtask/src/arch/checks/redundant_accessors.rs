//! Detect redundant data-access paths inside one type.
//!
//! Three patterns, all driven by AST data-flow analysis (no name matching):
//!
//! - **P1 — pub field + getter**: a struct exposes `pub x: T` and its `impl`
//!   block also has `pub fn x(&self) -> &T { &self.x }` (or `.clone()` flavour).
//!   External code now has two paths to the same datum.
//! - **P2 — nested shorthand**: an impl exposes both `pub fn a(&self) -> &Inner
//!   { &self.inner }` and `pub fn a_field(&self) -> &TypeA { &self.inner.field }`.
//!   The shorthand is a strict prefix-extension of the container path.
//! - **P3 — mutation handle alongside setter**: an impl exposes `&` to an
//!   interior-mutability container (`Atomic*`, `Mutex`, `RefCell`, ...) AND has
//!   another method that writes to the same field via `store`/`set`/...
//!   Both can mutate, defeating the encapsulation.
//!
//! ## How to fix P3
//!
//! The check counts **write-paths**, not their nominal visibility. There are
//! exactly two acceptable fixes:
//!
//! 1. **Remove the accessor.** Replace the pull-model handle with a push event
//!    fired by the legitimate setter (e.g. `bus.publish(VariantApplied { … })`)
//!    so consumers update their own local state.
//! 2. **Relocate ownership.** Move the field out of this type into the
//!    consumer; the setter becomes a command/event the consumer handles. The
//!    type is then a *decision engine* with no shared mutable state to leak.
//!
//! What does NOT fix it:
//!
//! - Wrapping the returned `Arc<Atomic*>` / `Arc<Mutex<_>>` in a "read-only"
//!   newtype. The newtype hides the write API but still hands out a cloneable
//!   `Arc` to the same atomic, which any holder can downcast or pattern-match
//!   to recover write access. The check counts how many places can mutate the
//!   field at runtime; a thin wrapper does not change that count.
//! - Renaming the accessor or marking it `#[doc(hidden)]`. The mutability
//!   capability is in the type returned, not in the symbol name.
//!
//! Findings are file-local; checking across files would need name resolution.

use std::collections::BTreeMap;

use anyhow::Result;

use super::{Check, Context};
use crate::{
    arch::config::AccessorSeverity,
    common::{
        parse::{
            AccessKind, AccessPath, PassthroughOpts, Scope, collect_scopes,
            collect_self_field_writes, extract_passthrough_with, is_strict_pub, parse_file,
            pub_methods, returns_handle_type, self_ty_name,
        },
        violation::Violation,
        walker::{relative_to, workspace_rs_files_scoped},
    },
};

pub(crate) const ID: &str = "redundant_accessors";

pub(crate) struct RedundantAccessors;

struct MethodFacts<'a> {
    name: String,
    fn_item: &'a syn::ImplItemFn,
    passthrough: Option<AccessPath>,
}

impl Check for RedundantAccessors {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.redundant_accessors;
        let opts = PassthroughOpts {
            wrapper_ctors: cfg.wrapper_ctors.clone(),
            expose_methods: cfg.expose_methods.clone(),
        };
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");

            for scope in collect_scopes(&file) {
                analyze_scope(cfg, &opts, &rel, &scope, &mut violations);
            }
        }

        violations.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.message.cmp(&b.message)));
        violations.dedup_by(|a, b| a.key == b.key && a.message == b.message);
        Ok(violations)
    }
}

fn analyze_scope(
    cfg: &crate::arch::config::RedundantAccessorsThreshold,
    opts: &PassthroughOpts,
    rel: &str,
    scope: &Scope<'_>,
    out: &mut Vec<Violation>,
) {
    // Build pub-fields map for structs in this scope.
    let mut pub_fields_by_type: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in &scope.structs {
        if let syn::Fields::Named(named) = &s.fields {
            for f in &named.named {
                if is_strict_pub(&f.vis)
                    && let Some(id) = &f.ident
                {
                    pub_fields_by_type
                        .entry(s.ident.to_string())
                        .or_default()
                        .push(id.to_string());
                }
            }
        }
    }

    // Aggregate impl blocks by target type — covers `impl X` + `impl Trait for X`.
    let mut by_target: BTreeMap<String, Vec<&syn::ItemImpl>> = BTreeMap::new();
    for &im in &scope.impls {
        if cfg.ignore_deref && is_deref_impl(im) {
            continue;
        }
        let Some(name) = self_ty_name(&im.self_ty) else {
            continue;
        };
        by_target.entry(name).or_default().push(im);
    }

    let mod_prefix = if scope.path.is_empty() {
        String::new()
    } else {
        format!("{}::", scope.path.join("::"))
    };

    for (target_type, impls) in by_target {
        let methods: Vec<MethodFacts<'_>> = impls
            .iter()
            .flat_map(|im| collect_method_facts(im, cfg.public_only, opts))
            .collect();

        if cfg.detect_field_passthrough {
            detect_p1(
                cfg,
                rel,
                &mod_prefix,
                &target_type,
                &pub_fields_by_type,
                &methods,
                out,
            );
        }
        if cfg.detect_nested_shorthand {
            detect_p2(cfg, rel, &mod_prefix, &target_type, &methods, out);
        }
        if cfg.detect_mutation_handle {
            detect_p3(cfg, rel, &mod_prefix, &target_type, &methods, out);
        }
    }
}

fn collect_method_facts<'a>(
    impl_block: &'a syn::ItemImpl,
    public_only: bool,
    opts: &PassthroughOpts,
) -> Vec<MethodFacts<'a>> {
    pub_methods(impl_block)
        .filter(|f| !public_only || is_strict_pub(&f.vis))
        .map(|f| MethodFacts {
            name: f.sig.ident.to_string(),
            fn_item: f,
            passthrough: extract_passthrough_with(f, opts),
        })
        .collect()
}

fn detect_p1(
    cfg: &crate::arch::config::RedundantAccessorsThreshold,
    rel: &str,
    mod_prefix: &str,
    target_type: &str,
    pub_fields_by_type: &BTreeMap<String, Vec<String>>,
    methods: &[MethodFacts<'_>],
    out: &mut Vec<Violation>,
) {
    let Some(pub_fields) = pub_fields_by_type.get(target_type) else {
        return;
    };
    for fname in pub_fields {
        for m in methods {
            let Some(p) = &m.passthrough else { continue };
            if p.fields.as_slice() == [fname.clone()] {
                let key = format!("{rel}::{mod_prefix}{target_type}::{}", m.name);
                let msg = format!(
                    "P1: pub field `{fname}` is also exposed via `pub fn {}(&self)` ({:?}); \
                     pick one path",
                    m.name, p.kind
                );
                out.push(emit(cfg.p1_severity, key, msg));
            }
        }
    }
}

fn detect_p2(
    cfg: &crate::arch::config::RedundantAccessorsThreshold,
    rel: &str,
    mod_prefix: &str,
    target_type: &str,
    methods: &[MethodFacts<'_>],
    out: &mut Vec<Violation>,
) {
    let with_paths: Vec<(&str, &AccessPath)> = methods
        .iter()
        .filter_map(|m| m.passthrough.as_ref().map(|p| (m.name.as_str(), p)))
        .collect();

    for &(short_name, short_path) in &with_paths {
        if short_path.fields.len() < 2 {
            continue;
        }
        for &(container_name, container_path) in &with_paths {
            if container_name == short_name {
                continue;
            }
            if container_path.fields.len() >= short_path.fields.len()
                || !short_path.fields.starts_with(&container_path.fields)
            {
                continue;
            }
            let key = format!("{rel}::{mod_prefix}{target_type}::{short_name}");
            let chain = short_path.fields.join(".");
            let inner = container_path.fields.join(".");
            let tail = short_path.fields[container_path.fields.len()..].join(".");
            let msg = format!(
                "P2: `{short_name}` is a shorthand for `{container_name}().{tail}` \
                 (data path `self.{chain}` extends `self.{inner}`); pick one path",
            );
            out.push(emit(cfg.p2_severity, key, msg));
        }
    }
}

fn detect_p3(
    cfg: &crate::arch::config::RedundantAccessorsThreshold,
    rel: &str,
    mod_prefix: &str,
    target_type: &str,
    methods: &[MethodFacts<'_>],
    out: &mut Vec<Violation>,
) {
    for m in methods {
        let Some(p) = &m.passthrough else { continue };
        if p.fields.len() != 1
            || !matches!(
                p.kind,
                AccessKind::Ref | AccessKind::Clone | AccessKind::Move
            )
        {
            continue;
        }
        if !returns_handle_type(&m.fn_item.sig, &cfg.mutable_handle_types) {
            continue;
        }
        let exposed_field = &p.fields[0];

        for other in methods {
            if other.name == m.name {
                continue;
            }
            let writes = collect_self_field_writes(other.fn_item, &cfg.writer_methods);
            if writes.iter().any(|w| w == exposed_field) {
                let key = format!("{rel}::{mod_prefix}{target_type}::{}", m.name);
                let msg = format!(
                    "P3: `{}` returns a handle to `self.{exposed_field}` (interior mutability) \
                     while `{}` already mutates the same field; external mutation through \
                     the handle bypasses the setter. \
                     Fix the *count* of write-paths, not their visibility: \
                     (a) delete this accessor and replace pull with a push event from the \
                     legitimate setter, or (b) move ownership of `{exposed_field}` to the \
                     consumer so the setter becomes a command. \
                     A read-only newtype wrapper around the same `Arc` does NOT fix this — \
                     it hides the second write-path behind a thin facade while the underlying \
                     `Arc<Atomic*/Mutex<...>>` is still cloneable and writable.",
                    m.name, other.name
                );
                out.push(emit(cfg.p3_severity, key, msg));
                break;
            }
        }
    }
}

fn emit(sev: AccessorSeverity, key: String, message: String) -> Violation {
    match sev {
        AccessorSeverity::Deny => Violation::deny(ID, key, message),
        AccessorSeverity::Warn => Violation::warn(ID, key, message),
        AccessorSeverity::Off => unreachable!("off severity should be filtered earlier"),
    }
}

fn is_deref_impl(im: &syn::ItemImpl) -> bool {
    let Some((_, path, _)) = &im.trait_ else {
        return false;
    };
    let Some(last) = path.segments.last() else {
        return false;
    };
    matches!(last.ident.to_string().as_str(), "Deref" | "DerefMut")
}
