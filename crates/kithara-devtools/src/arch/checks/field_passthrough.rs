use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use quote::ToTokens;
use syn::{Field, Fields, GenericArgument, Item, ItemStruct, PathArguments, Type, TypePath};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    suppress::Suppressions,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "field_passthrough";

struct Consts;

impl Consts {
    const MAX_DEPTH: usize = 8;
    const TRANSPARENT_WRAPPERS: &'static [&'static str] =
        &["Arc", "Rc", "Box", "RefCell", "Cell", "Mutex", "RwLock"];
}

pub(crate) struct FieldPassthrough;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FieldSig {
    name: String,
    type_sig: String,
}

#[derive(Debug)]
struct StructField {
    sig: FieldSig,
    leaf_struct: Option<String>,
    has_delegate_attr: bool,
}

#[derive(Debug)]
struct StructDecl {
    rel: String,
    line: usize,
    name: String,
    fields: Vec<StructField>,
}

impl Check for FieldPassthrough {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.field_passthrough;
        let exempt = compile_globs(&cfg.exempt_files);
        let mut all: Vec<StructDecl> = Vec::new();
        let mut suppressions: HashMap<String, Suppressions> = HashMap::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path);
            if matches_any(&exempt, rel) {
                continue;
            }
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let src = std::fs::read_to_string(&path)?;
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            suppressions.insert(rel_str.clone(), Suppressions::parse(&src));
            collect_structs(&rel_str, &file.items, &mut all);
        }
        let mut out = Vec::new();
        emit_violations(&all, &suppressions, &mut out);
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }
}

fn collect_structs(rel: &str, items: &[Item], out: &mut Vec<StructDecl>) {
    for item in items {
        match item {
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect_structs(rel, inner, out);
                }
            }
            Item::Struct(s) => {
                if let Some(decl) = struct_decl(rel, s) {
                    out.push(decl);
                }
            }
            _ => {}
        }
    }
}

fn struct_decl(rel: &str, s: &ItemStruct) -> Option<StructDecl> {
    let Fields::Named(named) = &s.fields else {
        return None;
    };
    let mut fields = Vec::new();
    for f in &named.named {
        let Some(ident) = &f.ident else { continue };
        let peeled = peel_wrappers(&f.ty);
        fields.push(StructField {
            sig: FieldSig {
                name: ident.to_string(),
                type_sig: render_type(&peeled),
            },
            leaf_struct: leaf_struct_name(&peeled),
            has_delegate_attr: has_delegate_attr(f),
        });
    }
    Some(StructDecl {
        rel: rel.to_string(),
        line: s.ident.span().start().line,
        name: s.ident.to_string(),
        fields,
    })
}

fn has_delegate_attr(f: &Field) -> bool {
    f.attrs.iter().any(|a| a.path().is_ident("delegate"))
}

/// Strip `&T`, `&mut T`, and the transparent owning wrappers (`Arc<T>`,
/// `Rc<T>`, `Box<T>`, `RefCell<T>`, `Cell<T>`, `Mutex<T>`, `RwLock<T>`).
fn peel_wrappers(ty: &Type) -> Type {
    match ty {
        Type::Reference(r) => peel_wrappers(&r.elem),
        Type::Path(tp) => {
            let Some(seg) = tp.path.segments.last() else {
                return ty.clone();
            };
            if !Consts::TRANSPARENT_WRAPPERS.contains(&seg.ident.to_string().as_str()) {
                return ty.clone();
            }
            let PathArguments::AngleBracketed(args) = &seg.arguments else {
                return ty.clone();
            };
            let inner = args.args.iter().find_map(|a| match a {
                GenericArgument::Type(t) => Some(t),
                _ => None,
            });
            inner.map_or_else(|| ty.clone(), peel_wrappers)
        }
        _ => ty.clone(),
    }
}

/// Normalised token string for type-equality comparison.
fn render_type(ty: &Type) -> String {
    ty.to_token_stream()
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Last path segment of a peeled `TypePath`, if any. Returns None for
/// compound types with multiple type args (`HashMap<K, V>`).
fn leaf_struct_name(ty: &Type) -> Option<String> {
    let Type::Path(TypePath { path, .. }) = ty else {
        return None;
    };
    let seg = path.segments.last()?;
    if let PathArguments::AngleBracketed(args) = &seg.arguments
        && args
            .args
            .iter()
            .filter(|a| matches!(a, GenericArgument::Type(_)))
            .count()
            > 1
    {
        return None;
    }
    Some(seg.ident.to_string())
}

fn reachable_sigs<'a>(
    start: &str,
    by_name: &BTreeMap<&'a str, &'a StructDecl>,
    out: &mut HashSet<FieldSig>,
    visited: &mut HashSet<String>,
    depth: usize,
) {
    if depth >= Consts::MAX_DEPTH {
        return;
    }
    if !visited.insert(start.to_string()) {
        return;
    }
    let Some(decl) = by_name.get(start) else {
        return;
    };
    for f in &decl.fields {
        out.insert(f.sig.clone());
        if let Some(leaf) = &f.leaf_struct {
            reachable_sigs(leaf, by_name, out, visited, depth + 1);
        }
    }
}

fn emit_violations(
    all: &[StructDecl],
    suppressions: &HashMap<String, Suppressions>,
    out: &mut Vec<Violation>,
) {
    let by_name: BTreeMap<&str, &StructDecl> = all.iter().map(|d| (d.name.as_str(), d)).collect();
    let empty = Suppressions::default();
    for outer in all {
        for outer_field in &outer.fields {
            let Some(inner_struct) = &outer_field.leaf_struct else {
                continue;
            };
            if !by_name.contains_key(inner_struct.as_str()) {
                continue;
            }
            let mut reachable: HashSet<FieldSig> = HashSet::new();
            let mut visited: HashSet<String> = HashSet::new();
            visited.insert(outer.name.clone());
            reachable_sigs(inner_struct, &by_name, &mut reachable, &mut visited, 0);
            for peer in &outer.fields {
                if peer.sig.name == outer_field.sig.name {
                    continue;
                }
                if peer.has_delegate_attr {
                    continue;
                }
                if !reachable.contains(&peer.sig) {
                    continue;
                }
                let sup = suppressions.get(&outer.rel).unwrap_or(&empty);
                if sup.is_suppressed(outer.line, ID) {
                    continue;
                }
                let key = format!(
                    "{}:{}:{}::{}",
                    outer.rel, outer.line, outer.name, peer.sig.name
                );
                out.push(Violation::warn(
                    ID,
                    key,
                    format!(
                        "`{outer_struct}.{peer}: {ty}` duplicates the same-shape field already \
                         reachable through `self.{nested}` (`{inner_struct}` ⤳ … ⤳ `{peer}: {ty}`); \
                         drop the local copy and read it through the existing path, or use \
                         `#[delegate(...)]` for a forwarding shim.",
                        outer_struct = outer.name,
                        peer = peer.sig.name,
                        ty = peer.sig.type_sig,
                        nested = outer_field.sig.name,
                        inner_struct = inner_struct,
                    ),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(src: &str) -> usize {
        let suppress = Suppressions::parse(src);
        let mut suppressions = HashMap::new();
        suppressions.insert("fixture.rs".to_string(), suppress);
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let mut structs = Vec::new();
        collect_structs("fixture.rs", &file.items, &mut structs);
        let mut out = Vec::new();
        emit_violations(&structs, &suppressions, &mut out);
        out.len()
    }

    #[test]
    fn duplicated_field_one_level_flagged() {
        let src = "struct StoreOptions { flush_hub: u32 } \
                   struct ResourceConfig { store: StoreOptions, flush_hub: u32 }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn duplicated_field_two_levels_flagged() {
        let src = "struct Diagnostics { log_level: u8 } \
                   struct AudioConfig { diagnostics: Diagnostics } \
                   struct PlayerConfig { audio: AudioConfig } \
                   struct Player { config: PlayerConfig, log_level: u8 }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn duplicated_field_through_arc_flagged() {
        let src = "struct Inner { token: u32 } \
                   struct Outer { inner: std::sync::Arc<Inner>, token: u32 }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn duplicated_field_through_box_flagged() {
        let src = "struct Inner { id: u64 } \
                   struct Outer { inner: Box<Inner>, id: u64 }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn arc_wrapper_equivalent_to_bare_type_flagged() {
        let src = "struct Handle; \
                   struct Inner { handle: Handle } \
                   struct Outer { inner: Inner, handle: std::sync::Arc<Handle> }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn name_collision_with_type_mismatch_clean() {
        let src = "struct Inner { id: String } \
                   struct Outer { inner: Inner, id: u64 }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn unrelated_fields_clean() {
        let src = "struct Inner { a: u8, b: u8 } \
                   struct Outer { inner: Inner, c: u8 }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn vec_args_distinguish_types() {
        let src = "struct Inner { buf: Vec<u8> } \
                   struct Outer { inner: Inner, buf: Vec<u16> }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn vec_args_match_flagged() {
        let src = "struct Inner { buf: Vec<u8> } \
                   struct Outer { inner: Inner, buf: Vec<u8> }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn missing_inner_struct_skips() {
        let src = "struct Outer { inner: Unknown, x: u8 }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn cycle_does_not_loop() {
        let src = "struct A { b: Box<B>, only_a: u8 } \
                   struct B { a: Box<A>, only_b: u8 }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn delegate_attr_exempt() {
        let src = "struct Inner { token: u32 } \
                   struct Outer { inner: Inner, #[delegate(to = inner)] token: u32 }";
        assert_eq!(count(src), 0);
    }
}
