//! `Arc<Mutex<Collection>>` — coarse-grained locking around an entire
//! collection.
//!
//! Locking the whole collection serialises every read and every write,
//! even when callers touch disjoint keys. For HashMap-like collections
//! the workspace prefers `dashmap::DashMap` / `DashSet` (per-bucket
//! locking, no global serialisation point); for append-heavy `Vec`
//! workloads a lock-free `crossbeam::queue::SegQueue` or a
//! `tokio::sync::mpsc` channel is cheaper still.
//!
//! Detection: walks every `syn::Type::Path` and matches the trailing
//! shape `Arc < (Mutex | RwLock) < (HashMap | HashSet | BTreeMap |
//! BTreeSet | Vec) < ... > > >`. Path prefixes are ignored — the
//! check is structural, not symbol-table-aware. Cross-cutting
//! `kithara_platform::Mutex` and `parking_lot::Mutex` count the
//! same as `std::sync::Mutex`: the contention shape is identical.
//!
//! The existing `arch.no-arc-mutex-godmap.yml` ast-grep rule catches
//! one specific `HashMap` shape; this check is broader (`Vec`, `HashSet`,
//! `BTreeMap`, `BTreeSet`) and ships the long-form Why / Bad / Good /
//! Suppress block per finding.

use anyhow::Result;
use syn::{
    GenericArgument, Path, PathArguments, Type, TypePath,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    suppress::Suppressions,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "arc_mutex_collection";

const EXPLANATION: &str = "\
Detected `Arc<Mutex<Collection>>` (or `Arc<RwLock<Collection>>`) wrapping \
a HashMap / HashSet / Vec / BTreeMap / BTreeSet.

Why it matters. Coarse-grained locks around a whole collection serialise \
every read and every write, even when callers touch disjoint keys. Lock \
contention on a single shared `Mutex<HashMap>` is a top-3 source of \
latency stalls in audio pipelines (the holder is preempted while the \
renderer waits). Per-bucket locking via `dashmap::DashMap` / `DashSet` \
removes the global serialisation point — each bucket has its own lock, \
callers on different keys never block each other. For pure append-heavy \
collections (telemetry, work queues), a lock-free `crossbeam::queue::SegQueue` \
or `tokio::sync::mpsc` channel is even cheaper.

❌  decrypted_keys: Arc<Mutex<HashMap<Url, Bytes>>>,        // global lock per lookup
✅  decrypted_keys: Arc<dashmap::DashMap<Url, Bytes>>,      // bucket-level locking
✅  decrypted_keys: Arc<RwLock<HashMap<Url, Bytes>>>,       // many concurrent readers

Suppress with `// xtask-lint-ignore: arc_mutex_collection` when:
1. The collection is mutated rarely under exclusive write (e.g. config
   built once at startup, then read-only).
2. The access pattern requires consistent multi-key transactions (DashMap
   bucket locks don't compose).
3. wasm32 builds where DashMap pulls dependencies that don't compile.";

pub(crate) struct ArcMutexCollection;

impl Check for ArcMutexCollection {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.arc_mutex_collection;
        let exempt = compile_globs(&cfg.exempt_files);
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel_path = relative_to(ctx.workspace_root, &path).to_path_buf();
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            if matches_any(&exempt, std::path::Path::new(&rel)) {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let suppress = Suppressions::parse(&source);
            let mut v = TypeVisitor {
                rel: &rel,
                suppress: &suppress,
                out: &mut violations,
            };
            v.visit_file(&file);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

struct TypeVisitor<'a> {
    rel: &'a str,
    suppress: &'a Suppressions,
    out: &'a mut Vec<Violation>,
}

impl<'ast> Visit<'ast> for TypeVisitor<'_> {
    fn visit_type_path(&mut self, tp: &'ast TypePath) {
        if let Some(inner) = match_arc_outer(&tp.path)
            && let Some(inner) = match_lock_outer(inner)
            && let Some(coll) = match_collection(inner)
        {
            let s = tp.span().start();
            if !self.suppress.is_suppressed(s.line, ID) {
                let key = format!("{}:{}:{}", self.rel, s.line, s.column);
                let msg = format!(
                    "M1: `Arc<{lock}<{coll}<...>>>` — replace with {hint} for fine-grained locking",
                    lock = "Mutex|RwLock",
                    coll = coll.0,
                    hint = coll.1,
                );
                self.out
                    .push(Violation::warn(ID, key, msg).with_explanation(EXPLANATION));
            }
        }
        visit::visit_type_path(self, tp);
    }
}

/// If `path` is the outer `Arc<...>`, return the single inner type.
fn match_arc_outer(path: &Path) -> Option<&Type> {
    let last = path.segments.last()?;
    if last.ident != "Arc" {
        return None;
    }
    single_generic_type(&last.arguments)
}

/// If `ty` is `Mutex<...>` / `RwLock<...>`, return the inner type.
fn match_lock_outer(ty: &Type) -> Option<&Type> {
    let Type::Path(tp) = ty else { return None };
    let last = tp.path.segments.last()?;
    let name = last.ident.to_string();
    if !matches!(name.as_str(), "Mutex" | "RwLock") {
        return None;
    }
    single_generic_type(&last.arguments)
}

/// If `ty` is one of the watched collections, return `(name, suggestion)`.
fn match_collection(ty: &Type) -> Option<(&'static str, &'static str)> {
    let Type::Path(tp) = ty else { return None };
    let last = tp.path.segments.last()?;
    let name = last.ident.to_string();
    Some(match name.as_str() {
        "HashMap" => ("HashMap", "`dashmap::DashMap`"),
        "HashSet" => ("HashSet", "`dashmap::DashSet`"),
        "BTreeMap" => (
            "BTreeMap",
            "`dashmap::DashMap` (if order isn't required) or per-key Mutex",
        ),
        "BTreeSet" => (
            "BTreeSet",
            "`dashmap::DashSet` (if order isn't required) or per-key Mutex",
        ),
        "Vec" => (
            "Vec",
            "`crossbeam::queue::SegQueue` (append-heavy) or `tokio::sync::mpsc` channel",
        ),
        _ => return None,
    })
}

fn single_generic_type(args: &PathArguments) -> Option<&Type> {
    let PathArguments::AngleBracketed(angle) = args else {
        return None;
    };
    let mut iter = angle.args.iter().filter_map(|arg| match arg {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    let first = iter.next()?;
    if iter.next().is_some() {
        return None;
    }
    Some(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_in(src: &str) -> usize {
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let suppress = Suppressions::parse(src);
        let mut out = Vec::new();
        let mut v = TypeVisitor {
            rel: "fixture.rs",
            suppress: &suppress,
            out: &mut out,
        };
        v.visit_file(&file);
        out.len()
    }

    #[test]
    fn arc_mutex_hashmap_flagged() {
        let src = "use std::sync::{Arc, Mutex}; use std::collections::HashMap; struct S { m: Arc<Mutex<HashMap<u64, String>>> }";
        assert_eq!(count_in(src), 1);
    }

    #[test]
    fn arc_rwlock_hashmap_flagged() {
        let src = "use std::sync::{Arc, RwLock}; use std::collections::HashMap; struct S { m: Arc<RwLock<HashMap<u64, String>>> }";
        assert_eq!(count_in(src), 1);
    }

    #[test]
    fn arc_mutex_vec_flagged() {
        let src = "use std::sync::{Arc, Mutex}; struct S { v: Arc<Mutex<Vec<u8>>> }";
        assert_eq!(count_in(src), 1);
    }

    #[test]
    fn arc_mutex_hashset_flagged() {
        let src = "use std::sync::{Arc, Mutex}; use std::collections::HashSet; struct S { s: Arc<Mutex<HashSet<u64>>> }";
        assert_eq!(count_in(src), 1);
    }

    #[test]
    fn arc_mutex_btreemap_flagged() {
        let src = "use std::sync::{Arc, Mutex}; use std::collections::BTreeMap; struct S { m: Arc<Mutex<BTreeMap<u64, u8>>> }";
        assert_eq!(count_in(src), 1);
    }

    #[test]
    fn arc_alone_not_flagged() {
        let src = "use std::sync::Arc; struct S { x: Arc<u32> }";
        assert_eq!(count_in(src), 0);
    }

    #[test]
    fn mutex_alone_not_flagged() {
        let src = "use std::sync::Mutex; use std::collections::HashMap; struct S { m: Mutex<HashMap<u64, u8>> }";
        assert_eq!(count_in(src), 0);
    }

    #[test]
    fn arc_mutex_struct_not_flagged() {
        let src = "use std::sync::{Arc, Mutex}; struct Inner; struct S { m: Arc<Mutex<Inner>> }";
        assert_eq!(count_in(src), 0);
    }

    #[test]
    fn fully_qualified_path_works() {
        let src = "struct S { m: ::std::sync::Arc<::std::sync::Mutex<::std::collections::HashMap<u64, u8>>> }";
        assert_eq!(count_in(src), 1);
    }

    #[test]
    fn suppression_works() {
        let src = "use std::sync::{Arc, Mutex}; use std::collections::HashMap;
struct S {
    // xtask-lint-ignore: arc_mutex_collection
    m: Arc<Mutex<HashMap<u64, String>>>,
}";
        assert_eq!(count_in(src), 0);
    }
}
