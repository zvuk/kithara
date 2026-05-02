//! A type should have one canonical public path inside a crate.
//!
//! Two patterns are flagged:
//!
//! - **R1 — explicit duplicate**: the same target (`crate::path::T`) appears
//!   in `pub use` re-exports across two or more files in one crate. Pick one
//!   canonical mountpoint and drop the other.
//! - **R2 — associated-type leak**: a `pub use` re-export of a type that is
//!   already publicly reachable as `<PublicType as PublicTrait>::Assoc =
//!   ThatType` somewhere in the same crate. The associated type already
//!   surfaces the type through the public trait — the explicit re-export is
//!   a redundant second canonical name.
//!
//! Per-crate analysis. Cross-crate facade re-exports (e.g. `pub use
//! kithara_file::*` in the umbrella crate) are intentional and out of scope.
//! Detection is AST-only and does not perform full module resolution; it
//! relies on local-name lookups inside one crate's source tree, which
//! captures the common cases (a type's local name is unique) without false
//! positives from foreign-crate look-alikes.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use cargo_metadata::TargetKind;
use syn::{ImplItem, Item, ItemImpl, Type, UseTree, Visibility, spanned::Spanned};

use super::{Check, Context};
use crate::common::{
    parse::parse_file, scope::packages_in_scope, violation::Violation, walker::walk_rs_files,
};

pub(crate) const ID: &str = "redundant_reexport";

pub(crate) struct RedundantReexport;

impl Check for RedundantReexport {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.redundant_reexport;
        let exempt: BTreeSet<&str> = cfg.exempt.iter().map(String::as_str).collect();
        let detect_r1 = cfg.detect.iter().any(|s| s == "explicit_duplicate");
        let detect_r2 = cfg.detect.iter().any(|s| s == "associated_type_leak");

        let mut violations = Vec::new();

        let scoped_pkgs = packages_in_scope(ctx.metadata, ctx.scope);
        for pkg in scoped_pkgs {
            let crate_root = match pkg.targets.iter().find(|t| {
                t.kind.iter().any(|k| {
                    matches!(
                        k,
                        TargetKind::Lib | TargetKind::RLib | TargetKind::ProcMacro
                    )
                })
            }) {
                Some(t) => match t.src_path.parent() {
                    Some(p) => p.as_std_path().to_path_buf(),
                    None => continue,
                },
                None => continue,
            };

            let crate_name = pkg.name.replace('-', "_");
            let info = collect_crate(&crate_root, &crate_name)?;

            if detect_r1 {
                emit_r1(&info, ctx.workspace_root, &exempt, &mut violations);
            }
            if detect_r2 {
                emit_r2(&info, ctx.workspace_root, &exempt, &mut violations);
            }
        }

        violations.sort_by(|a, b| a.key.cmp(&b.key));
        violations.dedup_by(|a, b| a.key == b.key);
        Ok(violations)
    }
}

#[derive(Debug)]
struct CrateInfo {
    crate_name: String,
    /// All `pub use` records (strict `pub`, not `pub(crate)`/`pub(super)`):
    /// target canonical path → list of sites.
    pub_uses: BTreeMap<String, Vec<ReexportSite>>,
    /// Local types (struct/enum/trait/type alias) defined anywhere in the crate
    /// with `pub` or `pub(crate)` visibility — keyed by the bare ident.
    pub_local_types: BTreeSet<String>,
    /// `(impl_target_ident, assoc_type_ident, alias_ident, file, span)` —
    /// associated types of public traits on public-reachable types.
    assoc_types: Vec<AssocTypeRecord>,
}

#[derive(Debug, Clone)]
struct ReexportSite {
    file: String,
    ident: String,
    line: usize,
    col: usize,
    /// `true` when the `pub use` lives in `lib.rs` at the crate root (the
    /// primary mountpoint for a type's canonical name). `false` when it lives
    /// in a nested module (`pub mod internal`, `pub mod ffi`, ...) — an
    /// alternative mountpoint that introduces a second path.
    is_root_lib: bool,
}

#[derive(Debug, Clone)]
struct AssocTypeRecord {
    impl_target: String,
    assoc_name: String,
    alias_ident: String,
    file: String,
    line: usize,
    col: usize,
}

fn collect_crate(crate_root: &std::path::Path, crate_name: &str) -> Result<CrateInfo> {
    let mut info = CrateInfo {
        crate_name: crate_name.to_string(),
        pub_uses: BTreeMap::new(),
        pub_local_types: BTreeSet::new(),
        assoc_types: Vec::new(),
    };
    for path in walk_rs_files(crate_root)? {
        let Ok(file) = parse_file(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(crate_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        scan_items(&file.items, &rel, &mut info);
    }
    Ok(info)
}

fn scan_items(items: &[Item], file: &str, info: &mut CrateInfo) {
    let is_root_lib = file == "lib.rs";
    for item in items {
        match item {
            Item::Use(u) if matches!(u.vis, Visibility::Public(_)) => {
                let mut targets: Vec<(String, String)> = Vec::new();
                let s = u.span().start();
                walk_use_tree(&u.tree, &mut Vec::new(), &mut targets);
                for (canonical, ident) in targets {
                    let canonical = normalize_target(&canonical, &info.crate_name);
                    info.pub_uses
                        .entry(canonical)
                        .or_default()
                        .push(ReexportSite {
                            file: file.to_string(),
                            ident,
                            line: s.line,
                            col: s.column,
                            is_root_lib,
                        });
                }
            }
            Item::Struct(it) if is_pub_or_crate(&it.vis) => {
                info.pub_local_types.insert(it.ident.to_string());
            }
            Item::Enum(it) if is_pub_or_crate(&it.vis) => {
                info.pub_local_types.insert(it.ident.to_string());
            }
            Item::Trait(it) if is_pub_or_crate(&it.vis) => {
                info.pub_local_types.insert(it.ident.to_string());
            }
            Item::Type(it) if is_pub_or_crate(&it.vis) => {
                info.pub_local_types.insert(it.ident.to_string());
            }
            Item::Impl(im) => collect_impl_assoc(im, file, info),
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    scan_items(inner, file, info);
                }
            }
            _ => {}
        }
    }
}

fn is_pub_or_crate(v: &Visibility) -> bool {
    matches!(v, Visibility::Public(_) | Visibility::Restricted(_))
}

/// Recursively flatten a `UseTree` into `(target_path, ident)` pairs.
fn walk_use_tree(tree: &UseTree, prefix: &mut Vec<String>, out: &mut Vec<(String, String)>) {
    match tree {
        UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            walk_use_tree(&p.tree, prefix, out);
            prefix.pop();
        }
        UseTree::Group(g) => {
            for it in &g.items {
                walk_use_tree(it, prefix, out);
            }
        }
        UseTree::Name(n) => {
            let mut full = prefix.clone();
            full.push(n.ident.to_string());
            out.push((full.join("::"), n.ident.to_string()));
        }
        UseTree::Rename(r) => {
            let mut full = prefix.clone();
            full.push(r.ident.to_string());
            out.push((full.join("::"), r.rename.to_string()));
        }
        UseTree::Glob(_) => {
            // skip — globs are intentional facade pattern, not a single-target dup
        }
    }
}

/// Replace a leading `crate::` segment with the actual crate name so we can
/// compare paths emitted from different crates uniformly.
fn normalize_target(path: &str, crate_name: &str) -> String {
    path.strip_prefix("crate::")
        .map_or_else(|| path.to_string(), |rest| format!("{crate_name}::{rest}"))
}

fn collect_impl_assoc(im: &ItemImpl, file: &str, info: &mut CrateInfo) {
    let Some(target_ident) = type_ident(&im.self_ty) else {
        return;
    };
    for it in &im.items {
        let ImplItem::Type(ty_item) = it else {
            continue;
        };
        let Some(alias_ident) = type_ident(&ty_item.ty) else {
            continue;
        };
        let s = ty_item.ident.span().start();
        info.assoc_types.push(AssocTypeRecord {
            impl_target: target_ident.clone(),
            assoc_name: ty_item.ident.to_string(),
            alias_ident,
            file: file.to_string(),
            line: s.line,
            col: s.column,
        });
    }
}

fn type_ident(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        Type::Reference(r) => type_ident(&r.elem),
        Type::Paren(p) => type_ident(&p.elem),
        Type::Group(g) => type_ident(&g.elem),
        _ => None,
    }
}

fn emit_r1(
    info: &CrateInfo,
    workspace_root: &std::path::Path,
    exempt: &BTreeSet<&str>,
    out: &mut Vec<Violation>,
) {
    for (target, sites) in &info.pub_uses {
        let leaf = target.rsplit("::").next().unwrap_or(target);
        if exempt.contains(leaf) {
            continue;
        }
        // Re-exporting std/core types from multiple wrapper modules is a
        // common facade pattern, not a duplication.
        if target.starts_with("std::") || target.starts_with("core::") {
            continue;
        }
        let unique_files: BTreeSet<&str> = sites.iter().map(|s| s.file.as_str()).collect();
        if unique_files.len() < 2 {
            continue;
        }
        let mut sorted: Vec<&ReexportSite> = sites.iter().collect();
        sorted.sort_by_key(|s| (s.file.clone(), s.line));
        let first = sorted[0];
        let mountpoints = unique_files.iter().copied().collect::<Vec<_>>().join(", ");
        let key = format!(
            "{}/{}:{}:{}::{}",
            display_workspace_path(workspace_root, &info.crate_name),
            first.file,
            first.line,
            first.col,
            target,
        );
        let msg = format!(
            "R1: target `{target}` is `pub use`'d in {} different files ({mountpoints}); \
             pick one canonical mountpoint",
            unique_files.len(),
        );
        out.push(Violation::warn(ID, key, msg));
    }
}

fn emit_r2(
    info: &CrateInfo,
    workspace_root: &std::path::Path,
    exempt: &BTreeSet<&str>,
    out: &mut Vec<Violation>,
) {
    // For R2 we only flag types that are pub-use'd from an *alternative*
    // mountpoint (not the lib.rs root). Root-level re-exports are the natural
    // canonical name; pairing them with an associated-type surface is fine.
    // The redundancy appears when the same type is *also* mounted under
    // `pub mod foo` (e.g. `internal`, `ffi`, ...) — that adds a second name.
    let mut alt_mountpoint_idents: BTreeSet<String> = BTreeSet::new();
    for sites in info.pub_uses.values() {
        for s in sites {
            if !s.is_root_lib {
                alt_mountpoint_idents.insert(s.ident.clone());
            }
        }
    }

    for rec in &info.assoc_types {
        if exempt.contains(rec.alias_ident.as_str()) {
            continue;
        }
        // Only flag if the alias names a local crate type.
        if !info.pub_local_types.contains(&rec.alias_ident) {
            continue;
        }
        // The aliased type also has an alt-mountpoint pub-use → second canonical path.
        if !alt_mountpoint_idents.contains(&rec.alias_ident) {
            continue;
        }
        let key = format!(
            "{}/{}:{}:{}::{}::{}",
            display_workspace_path(workspace_root, &info.crate_name),
            rec.file,
            rec.line,
            rec.col,
            rec.impl_target,
            rec.assoc_name,
        );
        let msg = format!(
            "R2: `{}` is `pub use`'d explicitly AND surfaced as \
             `<{} as _>::{} = {}`; the associated type already exposes the \
             type through the public trait — drop the explicit re-export to \
             keep one canonical path",
            rec.alias_ident, rec.impl_target, rec.assoc_name, rec.alias_ident,
        );
        out.push(Violation::warn(ID, key, msg));
    }
}

fn display_workspace_path(_workspace_root: &std::path::Path, crate_name: &str) -> String {
    format!("crates/{}", crate_name.replace('_', "-"))
}
