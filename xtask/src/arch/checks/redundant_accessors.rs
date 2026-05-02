//! Detect redundant data-access paths inside one type.
//!
//! Four patterns, all driven by AST data-flow analysis (no name matching):
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
//! - **P4 — accessor + `delegate!` passthrough**: an impl exposes
//!   `pub fn x(&self) -> &T { &self.x }` (or `.clone()` flavour) AND has a
//!   `delegate! { to self.x { … } }` block in the same target type. External
//!   callers reach `self.x` both as a handle and as forwarded methods.
//!   Detection scans `delegate!` macro tokens for `to self.<field>` patterns
//!   at the top level (no macro expansion, no name matching).
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
            AccessKind, AccessPath, PassthroughOpts, collect_scopes, collect_self_field_writes,
            extract_passthrough_with, is_strict_pub, parse_file, pub_methods, returns_handle_type,
            self_ty_name,
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

/// One impl block contributing to a target type's slice, plus where it came
/// from (for diagnostic keys). A `target_type`'s full slice is the union of
/// these across every workspace file that targets it.
struct ImplSite<'a> {
    impl_block: &'a syn::ItemImpl,
    file_rel: String,
    /// Module path of the scope the impl was found in, e.g. `queue::access`.
    /// Used to build human-readable violation keys; not part of the
    /// type-identity key (which is just the `target_type` ident).
    mod_prefix: String,
}

/// `pub` named fields by struct ident, aggregated across the workspace. Used
/// by P1 to pair `pub field` with a getter that returns it; ident-only
/// keying mirrors how impl blocks are aggregated.
type PubFieldsByType = BTreeMap<String, Vec<String>>;

/// One method in a target type's slice, paired with the file and module
/// path it was found in. Detectors use the file/module fields only to
/// build human-readable violation keys.
type MethodEntry<'a> = (MethodFacts<'a>, &'a str, &'a str);

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

        // Step 1: parse every file in scope and own the AST so impl blocks
        // collected from different files outlive the per-file loop. All
        // detectors operate on the union, not on per-file slivers.
        let mut parsed: Vec<(String, syn::File)> = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");
            parsed.push((rel, file));
        }

        // Step 2: aggregate impl sites + pub fields across all files, keyed
        // by type ident. Pre-existing P1/P2/P3 limitation about cross-module
        // ident collisions stands — name resolution would be required.
        let mut sites_by_type: BTreeMap<String, Vec<ImplSite<'_>>> = BTreeMap::new();
        let mut pub_fields_by_type: PubFieldsByType = BTreeMap::new();
        for (rel, file) in &parsed {
            for scope in collect_scopes(file) {
                let mod_prefix = if scope.path.is_empty() {
                    String::new()
                } else {
                    format!("{}::", scope.path.join("::"))
                };
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
                for &im in &scope.impls {
                    if cfg.ignore_deref && is_deref_impl(im) {
                        continue;
                    }
                    let Some(name) = self_ty_name(&im.self_ty) else {
                        continue;
                    };
                    sites_by_type.entry(name).or_default().push(ImplSite {
                        impl_block: im,
                        file_rel: rel.clone(),
                        mod_prefix: mod_prefix.clone(),
                    });
                }
            }
        }

        // Step 3: run detectors over the workspace-wide slice of each type.
        let mut violations = Vec::new();
        for (target_type, sites) in &sites_by_type {
            analyze_target_type(
                cfg,
                &opts,
                target_type,
                sites,
                &pub_fields_by_type,
                &mut violations,
            );
        }

        violations.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.message.cmp(&b.message)));
        violations.dedup_by(|a, b| a.key == b.key && a.message == b.message);
        Ok(violations)
    }
}

fn analyze_target_type(
    cfg: &crate::arch::config::RedundantAccessorsThreshold,
    opts: &PassthroughOpts,
    target_type: &str,
    sites: &[ImplSite<'_>],
    pub_fields_by_type: &PubFieldsByType,
    out: &mut Vec<Violation>,
) {
    // Carry per-method origin (file_rel + mod_prefix) so violation keys
    // stay precise across the cross-file aggregation.
    let mut methods: Vec<MethodEntry<'_>> = Vec::new();
    let mut delegate_sites: Vec<(String, &str)> = Vec::new();
    for site in sites {
        for facts in collect_method_facts(site.impl_block, cfg.public_only, opts) {
            methods.push((facts, site.file_rel.as_str(), site.mod_prefix.as_str()));
        }
        for field in collect_delegate_targets(site.impl_block) {
            delegate_sites.push((field, site.file_rel.as_str()));
        }
    }

    if cfg.detect_field_passthrough {
        detect_p1(cfg, target_type, pub_fields_by_type, &methods, out);
    }
    if cfg.detect_nested_shorthand {
        detect_p2(cfg, target_type, &methods, out);
    }
    if cfg.detect_mutation_handle {
        detect_p3(cfg, target_type, &methods, out);
    }
    if cfg.detect_delegate_passthrough {
        detect_p4(cfg, target_type, &methods, &delegate_sites, out);
    }
}

/// Scan an `impl` block for `delegate! { to self.<field> { … } }` macro
/// invocations, returning the field names found at the top level. Recursion
/// into the brace-delimited body group is unnecessary — the `delegate` crate
/// places `to self.<field>` directly at the top of its macro body.
fn collect_delegate_targets(im: &syn::ItemImpl) -> Vec<String> {
    let mut out = Vec::new();
    for item in &im.items {
        let syn::ImplItem::Macro(macro_item) = item else {
            continue;
        };
        if !path_ends_with(&macro_item.mac.path, "delegate") {
            continue;
        }
        scan_to_self_field(macro_item.mac.tokens.clone(), &mut out);
    }
    out
}

fn path_ends_with(path: &syn::Path, name: &str) -> bool {
    path.segments.last().is_some_and(|seg| seg.ident == name)
}

fn scan_to_self_field(tokens: proc_macro2::TokenStream, out: &mut Vec<String>) {
    use proc_macro2::TokenTree;

    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, window) in toks.windows(4).enumerate() {
        let TokenTree::Ident(to_kw) = &window[0] else {
            continue;
        };
        if to_kw != "to" {
            continue;
        }
        let TokenTree::Ident(self_kw) = &window[1] else {
            continue;
        };
        if self_kw != "self" {
            continue;
        }
        let TokenTree::Punct(dot) = &window[2] else {
            continue;
        };
        if dot.as_char() != '.' {
            continue;
        }
        let TokenTree::Ident(field) = &window[3] else {
            continue;
        };
        // Reject chained paths like `to self.inner.player` — the `inner`
        // field is not the delegate target, the leaf is. Pairing the leaf
        // with an `inner()` accessor would also be wrong, so we just skip
        // the whole chain.
        if let Some(TokenTree::Punct(next)) = toks.get(i + 4)
            && next.as_char() == '.'
        {
            continue;
        }
        out.push(field.to_string());
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

fn method_key(target_type: &str, m: &MethodEntry<'_>) -> String {
    let (facts, file_rel, mod_prefix) = m;
    format!("{file_rel}::{mod_prefix}{target_type}::{}", facts.name)
}

fn detect_p1(
    cfg: &crate::arch::config::RedundantAccessorsThreshold,
    target_type: &str,
    pub_fields_by_type: &PubFieldsByType,
    methods: &[MethodEntry<'_>],
    out: &mut Vec<Violation>,
) {
    let Some(pub_fields) = pub_fields_by_type.get(target_type) else {
        return;
    };
    for fname in pub_fields {
        for m in methods {
            let Some(p) = &m.0.passthrough else { continue };
            if p.fields.as_slice() == [fname.clone()] {
                let key = method_key(target_type, m);
                let msg = format!(
                    "P1: pub field `{fname}` is also exposed via `pub fn {}(&self)` ({:?}); \
                     pick one path",
                    m.0.name, p.kind
                );
                out.push(emit(cfg.p1_severity, key, msg));
            }
        }
    }
}

fn detect_p2(
    cfg: &crate::arch::config::RedundantAccessorsThreshold,
    target_type: &str,
    methods: &[MethodEntry<'_>],
    out: &mut Vec<Violation>,
) {
    let with_paths: Vec<(&MethodEntry<'_>, &AccessPath)> = methods
        .iter()
        .filter_map(|m| m.0.passthrough.as_ref().map(|p| (m, p)))
        .collect();

    for &(short_m, short_path) in &with_paths {
        if short_path.fields.len() < 2 {
            continue;
        }
        for &(container_m, container_path) in &with_paths {
            if container_m.0.name == short_m.0.name {
                continue;
            }
            if container_path.fields.len() >= short_path.fields.len()
                || !short_path.fields.starts_with(&container_path.fields)
            {
                continue;
            }
            let key = method_key(target_type, short_m);
            let chain = short_path.fields.join(".");
            let inner = container_path.fields.join(".");
            let tail = short_path.fields[container_path.fields.len()..].join(".");
            let msg = format!(
                "P2: `{}` is a shorthand for `{}().{tail}` \
                 (data path `self.{chain}` extends `self.{inner}`); pick one path",
                short_m.0.name, container_m.0.name,
            );
            out.push(emit(cfg.p2_severity, key, msg));
        }
    }
}

fn detect_p3(
    cfg: &crate::arch::config::RedundantAccessorsThreshold,
    target_type: &str,
    methods: &[MethodEntry<'_>],
    out: &mut Vec<Violation>,
) {
    for m in methods {
        let Some(p) = &m.0.passthrough else { continue };
        if p.fields.len() != 1
            || !matches!(
                p.kind,
                AccessKind::Ref | AccessKind::Clone | AccessKind::Move
            )
        {
            continue;
        }
        if !returns_handle_type(&m.0.fn_item.sig, &cfg.mutable_handle_types) {
            continue;
        }
        let exposed_field = &p.fields[0];

        for other in methods {
            if other.0.name == m.0.name {
                continue;
            }
            let writes = collect_self_field_writes(other.0.fn_item, &cfg.writer_methods);
            if writes.iter().any(|w| w == exposed_field) {
                let key = method_key(target_type, m);
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
                    m.0.name, other.0.name
                );
                out.push(emit(cfg.p3_severity, key, msg));
                break;
            }
        }
    }
}

fn detect_p4(
    cfg: &crate::arch::config::RedundantAccessorsThreshold,
    target_type: &str,
    methods: &[MethodEntry<'_>],
    delegate_sites: &[(String, &str)],
    out: &mut Vec<Violation>,
) {
    for (field, delegate_file) in delegate_sites {
        for m in methods {
            let Some(p) = &m.0.passthrough else { continue };
            if p.fields.as_slice() != [field.clone()] {
                continue;
            }
            if !matches!(
                p.kind,
                AccessKind::Ref | AccessKind::Clone | AccessKind::Move
            ) {
                continue;
            }
            let key = method_key(target_type, m);
            let cross_file_note = if m.1 == *delegate_file {
                String::new()
            } else {
                format!(" (delegate lives in `{delegate_file}`)")
            };
            let msg = format!(
                "P4: `{field}` is exposed both via `pub fn {}(&self)` and via \
                 `delegate! {{ to self.{field} {{ … }} }}`{cross_file_note}; pick one path \
                 (drop the accessor and keep `delegate!`, or drop the macro \
                 and route external callers through the accessor)",
                m.0.name
            );
            out.push(emit(cfg.p4_severity, key, msg));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        arch::config::{AccessorSeverity, RedundantAccessorsThreshold},
        common::{parse::collect_scopes, violation::Severity},
    };

    /// Run only the P4 detector across one or more synthetic source files,
    /// returning the violations it produces. Pairs of `(file_rel, source)`
    /// let cross-file scenarios assert that the workspace-wide aggregation
    /// matches accessor and `delegate!` even when they live in different
    /// files. Other detectors are switched off so assertions stay focused.
    fn run_p4(sources: &[(&str, &str)]) -> Vec<Violation> {
        let cfg = RedundantAccessorsThreshold {
            detect_field_passthrough: false,
            detect_nested_shorthand: false,
            detect_mutation_handle: false,
            detect_delegate_passthrough: true,
            p1_severity: AccessorSeverity::Off,
            p2_severity: AccessorSeverity::Off,
            p3_severity: AccessorSeverity::Off,
            p4_severity: AccessorSeverity::Warn,
            ..RedundantAccessorsThreshold::default()
        };
        let opts = PassthroughOpts {
            wrapper_ctors: cfg.wrapper_ctors.clone(),
            expose_methods: cfg.expose_methods.clone(),
        };
        let parsed: Vec<(String, syn::File)> = sources
            .iter()
            .map(|(rel, src)| {
                (
                    (*rel).to_string(),
                    syn::parse_str(src).expect("parse source"),
                )
            })
            .collect();

        let mut sites_by_type: BTreeMap<String, Vec<ImplSite<'_>>> = BTreeMap::new();
        let mut pub_fields_by_type: PubFieldsByType = BTreeMap::new();
        for (rel, file) in &parsed {
            for scope in collect_scopes(file) {
                let mod_prefix = if scope.path.is_empty() {
                    String::new()
                } else {
                    format!("{}::", scope.path.join("::"))
                };
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
                for &im in &scope.impls {
                    if cfg.ignore_deref && is_deref_impl(im) {
                        continue;
                    }
                    let Some(name) = self_ty_name(&im.self_ty) else {
                        continue;
                    };
                    sites_by_type.entry(name).or_default().push(ImplSite {
                        impl_block: im,
                        file_rel: rel.clone(),
                        mod_prefix: mod_prefix.clone(),
                    });
                }
            }
        }

        let mut out = Vec::new();
        for (target_type, sites) in &sites_by_type {
            analyze_target_type(
                &cfg,
                &opts,
                target_type,
                sites,
                &pub_fields_by_type,
                &mut out,
            );
        }
        out
    }

    #[test]
    fn p4_flags_accessor_paired_with_delegate_same_file() {
        let v = run_p4(&[(
            "test.rs",
            r#"
            use std::sync::Arc;
            pub struct Q { player: Arc<P> }
            impl Q {
                pub fn player(&self) -> &Arc<P> { &self.player }
                delegate::delegate! {
                    to self.player {
                        pub fn play(&self);
                        pub fn pause(&self);
                    }
                }
            }
        "#,
        )]);
        assert_eq!(v.len(), 1, "expected exactly one P4 violation, got {v:?}");
        assert_eq!(v[0].severity, Severity::Warn);
        assert!(
            v[0].message.contains("P4:") && v[0].message.contains("`player`"),
            "unexpected message: {}",
            v[0].message
        );
    }

    #[test]
    fn p4_flags_accessor_paired_with_delegate_cross_file() {
        // Real-world shape: Queue's accessor lives in `access.rs`, the
        // `delegate!` block lives in `passthrough.rs`. A per-file pass
        // would miss this — the cross-file aggregation must catch it.
        let v = run_p4(&[
            (
                "crates/x/src/access.rs",
                r#"
                use std::sync::Arc;
                pub struct Q { player: Arc<P> }
                impl Q {
                    pub fn player(&self) -> &Arc<P> { &self.player }
                }
            "#,
            ),
            (
                "crates/x/src/passthrough.rs",
                r#"
                impl Q {
                    delegate::delegate! {
                        to self.player {
                            pub fn play(&self);
                        }
                    }
                }
            "#,
            ),
        ]);
        assert_eq!(v.len(), 1, "expected one cross-file P4, got {v:?}");
        assert!(
            v[0].message
                .contains("delegate lives in `crates/x/src/passthrough.rs`"),
            "diagnostic should pinpoint the delegate file: {}",
            v[0].message
        );
        assert!(
            v[0].key.contains("crates/x/src/access.rs"),
            "violation key should anchor to the accessor's file: {}",
            v[0].key
        );
    }

    #[test]
    fn p4_silent_when_only_delegate() {
        assert!(
            run_p4(&[(
                "test.rs",
                r#"
                    pub struct Q { player: u32 }
                    impl Q {
                        delegate::delegate! {
                            to self.player {
                                pub fn play(&self);
                            }
                        }
                    }
                "#,
            )])
            .is_empty()
        );
    }

    #[test]
    fn p4_silent_when_only_accessor() {
        assert!(
            run_p4(&[(
                "test.rs",
                r#"
                    use std::sync::Arc;
                    pub struct Q { player: Arc<P> }
                    impl Q {
                        pub fn player(&self) -> &Arc<P> { &self.player }
                    }
                "#,
            )])
            .is_empty()
        );
    }

    #[test]
    fn p4_flags_only_matching_target_in_multi_target_delegate() {
        let v = run_p4(&[(
            "test.rs",
            r#"
                use std::sync::Arc;
                pub struct Q { a: Arc<A>, b: Arc<B> }
                impl Q {
                    pub fn a(&self) -> &Arc<A> { &self.a }
                    delegate::delegate! {
                        to self.a {
                            pub fn alpha(&self);
                        }
                        to self.b {
                            pub fn beta(&self);
                        }
                    }
                }
            "#,
        )]);
        assert_eq!(v.len(), 1, "expected one P4 (for `a` only), got {v:?}");
        assert!(v[0].message.contains("`a`"));
    }

    #[test]
    fn p4_silent_for_chained_delegate_target() {
        let v = run_p4(&[(
            "test.rs",
            r#"
                use std::sync::Arc;
                pub struct Q { inner: Arc<I> }
                impl Q {
                    pub fn inner(&self) -> &Arc<I> { &self.inner }
                    delegate::delegate! {
                        to self.inner.player {
                            pub fn play(&self);
                        }
                    }
                }
            "#,
        )]);
        assert!(
            v.is_empty(),
            "chained `to self.inner.player` should not pair with `inner` accessor: {v:?}"
        );
    }

    #[test]
    fn p4_silent_for_non_delegate_macro_with_to_self_tokens() {
        // A non-`delegate` macro that happens to contain `to self.x` tokens
        // must not trigger P4 — we only scan macros whose path tail is
        // `delegate`.
        let v = run_p4(&[(
            "test.rs",
            r#"
                pub struct Q { x: u32 }
                impl Q {
                    pub fn x(&self) -> &u32 { &self.x }
                    some_other_macro! { to self.x { } }
                }
            "#,
        )]);
        assert!(
            v.is_empty(),
            "non-delegate macro must not produce P4: {v:?}"
        );
    }
}
