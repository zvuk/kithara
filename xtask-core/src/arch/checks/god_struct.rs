use std::collections::{BTreeMap, HashSet};

use anyhow::Result;
use syn::{Block, Expr, Fields, ImplItem, Item, ItemImpl, ItemStruct, Stmt, Type};

use super::{Check, Context};
use crate::common::{
    exclude::attrs_have_cfg_test,
    parse::parse_file,
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "god_struct";

pub(crate) struct GodStruct;

impl Check for GodStruct {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.god_struct;
        let std_traits: HashSet<&str> = cfg.std_traits.iter().map(String::as_str).collect();

        let mut scans: Vec<FileScan> = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");

            let mut structs: BTreeMap<String, usize> = BTreeMap::new();
            let mut methods: BTreeMap<String, usize> = BTreeMap::new();
            collect(&file.items, &std_traits, &mut structs, &mut methods);
            scans.push(FileScan {
                rel,
                structs,
                methods,
            });
        }

        let violations = aggregate(scans, cfg.warn)
            .into_iter()
            .map(|hit| {
                let msg = format!(
                    "{}: {} substantial methods, {} fields (warn threshold {})",
                    hit.name, hit.methods, hit.fields, cfg.warn
                );
                Violation::warn(ID, hit.key, msg)
            })
            .collect();
        Ok(violations)
    }
}

/// One file's contribution: structs defined here (`name -> field count`) and
/// substantial methods found here (`name -> count`). Thin forwarder/accessor
/// methods, std-trait conformance, and `#[cfg(test)]` items are already
/// filtered out by [`collect`].
struct FileScan {
    rel: String,
    structs: BTreeMap<String, usize>,
    methods: BTreeMap<String, usize>,
}

/// A flagged type after cross-file aggregation. `key` is `<def-file>::<name>`.
struct GodHit {
    key: String,
    name: String,
    fields: usize,
    methods: usize,
}

#[derive(Default)]
struct CrateScan {
    /// `name -> (file where the struct is defined, field count)`.
    structs: BTreeMap<String, (String, usize)>,
    /// `name -> substantial methods summed across this crate's files`.
    methods: BTreeMap<String, usize>,
}

/// Aggregate a type's substantial-method count across every file of its owning
/// crate, so a god type whose `impl` blocks are spread over sibling files is
/// counted once by its true behaviour surface — and moving methods between
/// files cannot lower the count. Scoped per crate (`crates/<name>`), never
/// workspace-wide, so two same-named types in different crates stay separate.
///
/// The metric is substantial methods only. Field count is reported for context
/// but is owned by `pub_struct_open_fields`; thin forwarders/accessors are
/// idiomatic plumbing, not concentrated responsibility (see [`is_thin`]).
fn aggregate(scans: Vec<FileScan>, warn: usize) -> Vec<GodHit> {
    let mut crates: BTreeMap<String, CrateScan> = BTreeMap::new();
    for scan in scans {
        let entry = crates.entry(crate_key(&scan.rel).to_string()).or_default();
        for (name, fields) in scan.structs {
            entry
                .structs
                .entry(name)
                .or_insert_with(|| (scan.rel.clone(), fields));
        }
        for (name, n) in scan.methods {
            *entry.methods.entry(name).or_insert(0) += n;
        }
    }

    let mut hits = Vec::new();
    for entry in crates.values() {
        for (name, (def_rel, fields)) in &entry.structs {
            let methods = entry.methods.get(name).copied().unwrap_or(0);
            if methods >= warn {
                hits.push(GodHit {
                    key: format!("{def_rel}::{name}"),
                    name: name.clone(),
                    fields: *fields,
                    methods,
                });
            }
        }
    }
    hits
}

/// Owning-crate root (`crates/<name>`) used to scope per-type aggregation.
/// Paths outside `crates/` fall back to the file itself (per-file scope).
fn crate_key(rel: &str) -> &str {
    if let Some(rest) = rel.strip_prefix("crates/")
        && let Some(slash) = rest.find('/')
    {
        return &rel[.."crates/".len() + slash];
    }
    rel
}

fn collect(
    items: &[Item],
    std_traits: &HashSet<&str>,
    structs: &mut BTreeMap<String, usize>,
    methods: &mut BTreeMap<String, usize>,
) {
    for item in items {
        match item {
            Item::Struct(s) if attrs_have_cfg_test(&s.attrs) => {}
            Item::Struct(s) => {
                structs.insert(s.ident.to_string(), count_fields(s));
            }
            Item::Impl(im) if attrs_have_cfg_test(&im.attrs) => {}
            Item::Impl(im) if trait_name(im).is_some_and(|t| std_traits.contains(t.as_str())) => {}
            Item::Impl(im) => {
                if let Some(name) = self_ty_name(im) {
                    let n = im
                        .items
                        .iter()
                        .filter(|it| match it {
                            ImplItem::Fn(f) => !attrs_have_cfg_test(&f.attrs) && !is_thin(&f.block),
                            _ => false,
                        })
                        .count();
                    *methods.entry(name).or_insert(0) += n;
                }
            }
            Item::Mod(m) if attrs_have_cfg_test(&m.attrs) => {}
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect(inner, std_traits, structs, methods);
                }
            }
            _ => {}
        }
    }
}

/// A method body is "thin" — a forwarder or accessor, not concentrated
/// behaviour — when it has at most three statements and no top-level control
/// flow. Such methods (`fn x(&self) -> u32 { self.x.load(..) }`,
/// `fn pause(&self) { self.send(..) }`) are idiomatic facade plumbing and do
/// not count toward the god-struct total.
fn is_thin(block: &Block) -> bool {
    block.stmts.len() <= 3 && !block.stmts.iter().any(stmt_has_control_flow)
}

fn stmt_has_control_flow(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Expr(e, _) => expr_is_control_flow(e),
        Stmt::Local(l) => l
            .init
            .as_ref()
            .is_some_and(|i| expr_is_control_flow(&i.expr)),
        _ => false,
    }
}

fn expr_is_control_flow(e: &Expr) -> bool {
    matches!(
        e,
        Expr::If(_) | Expr::Match(_) | Expr::While(_) | Expr::ForLoop(_) | Expr::Loop(_)
    )
}

fn count_fields(s: &ItemStruct) -> usize {
    match &s.fields {
        Fields::Named(n) => n.named.len(),
        Fields::Unnamed(u) => u.unnamed.len(),
        Fields::Unit => 0,
    }
}

fn self_ty_name(im: &ItemImpl) -> Option<String> {
    match im.self_ty.as_ref() {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

fn trait_name(im: &ItemImpl) -> Option<String> {
    im.trait_
        .as_ref()
        .and_then(|(_, path, _)| path.segments.last())
        .map(|s| s.ident.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns `(fields, substantial_methods)` for `name` in a single source.
    fn counts(src: &str, name: &str) -> (usize, usize) {
        let std_traits: HashSet<&str> = ["Drop", "Default", "Debug", "From", "Add", "Ord"]
            .into_iter()
            .collect();
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let mut structs = BTreeMap::new();
        let mut methods = BTreeMap::new();
        collect(&file.items, &std_traits, &mut structs, &mut methods);
        (
            structs.get(name).copied().unwrap_or(0),
            methods.get(name).copied().unwrap_or(0),
        )
    }

    #[test]
    fn substantial_methods_count_thin_forwarders_do_not() {
        // `tall` has a 4-statement body; `forward`/`get` are thin plumbing.
        let src = "struct S { a: u8 }\n\
                   impl S {\n\
                       fn tall(&self) { let a = 1; let b = 2; let c = 3; let _ = a + b + c; }\n\
                       fn forward(&self) { self.inner.send(); }\n\
                       fn get(&self) -> u8 { self.a }\n\
                   }\n";
        assert_eq!(counts(src, "S"), (1, 1));
    }

    #[test]
    fn control_flow_makes_a_short_method_substantial() {
        let src = "struct S;\n\
                   impl S {\n\
                       fn branchy(&self) { if self.ok() { self.go() } }\n\
                       fn loopy(&self) { for x in self.iter() { drop(x) } }\n\
                       fn thin(&self) { self.x() }\n\
                   }\n";
        assert_eq!(counts(src, "S"), (0, 2));
    }

    #[test]
    fn cfg_test_impls_and_methods_are_excluded() {
        let src = "struct S;\n\
                   impl S { fn prod(&self) { if self.a() { self.b() } } #[cfg(test)] fn helper(&self) { if x() { y() } } }\n\
                   #[cfg(test)]\n\
                   impl S { fn t1(&self) { if a() { b() } } }\n";
        assert_eq!(counts(src, "S"), (0, 1));
    }

    #[test]
    fn std_trait_methods_excluded_domain_trait_methods_counted() {
        let src = "struct S;\n\
                   impl std::ops::Add for S { type Output = S; fn add(self, o: S) -> S { if x() { o } else { o } } }\n\
                   impl Decoder for S { fn decode(&self) { while a() { b() } } fn reset(&self) { for _ in z() { w() } } }\n";
        // Add is std plumbing (excluded); both Decoder methods are substantial.
        assert_eq!(counts(src, "S"), (0, 2));
    }

    fn scan(rel: &str, structs: &[(&str, usize)], methods: &[(&str, usize)]) -> FileScan {
        FileScan {
            rel: rel.to_string(),
            structs: structs
                .iter()
                .map(|(n, c)| ((*n).to_string(), *c))
                .collect(),
            methods: methods
                .iter()
                .map(|(n, c)| ((*n).to_string(), *c))
                .collect(),
        }
    }

    fn hit(hits: &[GodHit], key: &str) -> Option<(usize, usize)> {
        hits.iter()
            .find(|h| h.key == key)
            .map(|h| (h.fields, h.methods))
    }

    #[test]
    fn methods_aggregate_across_a_crates_files() {
        let scans = vec![
            scan(
                "crates/kithara-queue/src/queue/state.rs",
                &[("Queue", 6)],
                &[("Queue", 9)],
            ),
            scan(
                "crates/kithara-queue/src/queue/access.rs",
                &[],
                &[("Queue", 8)],
            ),
        ];
        let hits = aggregate(scans, 15);
        // 9 + 8 = 17 substantial methods, keyed by the struct-def file, once.
        assert_eq!(
            hit(&hits, "crates/kithara-queue/src/queue/state.rs::Queue"),
            Some((6, 17))
        );
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn moving_a_method_between_files_does_not_change_the_count() {
        let before = aggregate(
            vec![
                scan("crates/x/src/a.rs", &[("G", 3)], &[("G", 12)]),
                scan("crates/x/src/b.rs", &[], &[("G", 4)]),
            ],
            15,
        );
        let after = aggregate(
            vec![
                scan("crates/x/src/a.rs", &[("G", 3)], &[("G", 6)]),
                scan("crates/x/src/b.rs", &[], &[("G", 10)]),
            ],
            15,
        );
        assert_eq!(hit(&before, "crates/x/src/a.rs::G"), Some((3, 16)));
        assert_eq!(hit(&after, "crates/x/src/a.rs::G"), Some((3, 16)));
    }

    #[test]
    fn fields_alone_do_not_flag_a_struct() {
        // 30 fields, only 2 substantial methods: a config/state struct, not a
        // behaviour god — owned by `pub_struct_open_fields`, not god_struct.
        let scans = vec![scan("crates/x/src/cfg.rs", &[("Cfg", 30)], &[("Cfg", 2)])];
        assert!(aggregate(scans, 15).is_empty());
    }

    #[test]
    fn same_name_in_different_crates_stays_separate() {
        let scans = vec![
            scan("crates/a/src/lib.rs", &[("Job", 0)], &[("Job", 20)]),
            scan("crates/b/src/lib.rs", &[("Job", 2)], &[("Job", 3)]),
        ];
        let hits = aggregate(scans, 15);
        assert_eq!(hit(&hits, "crates/a/src/lib.rs::Job"), Some((0, 20)));
        assert_eq!(hit(&hits, "crates/b/src/lib.rs::Job"), None);
    }

    #[test]
    fn crate_key_scopes_to_crate_root_else_whole_path() {
        assert_eq!(
            crate_key("crates/kithara-hls/src/variant.rs"),
            "crates/kithara-hls"
        );
        assert_eq!(crate_key("xtask/src/main.rs"), "xtask/src/main.rs");
    }
}
