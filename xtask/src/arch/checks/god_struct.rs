use std::collections::{BTreeMap, HashSet};

use anyhow::Result;
use syn::{Fields, ImplItem, Item, ItemImpl, ItemStruct, Type};

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
            let mut impl_fns: BTreeMap<String, usize> = BTreeMap::new();
            collect(&file.items, &std_traits, &mut structs, &mut impl_fns);
            scans.push(FileScan {
                rel,
                structs,
                impl_fns,
            });
        }

        let violations = aggregate(scans, cfg.warn)
            .into_iter()
            .map(|hit| {
                let total = hit.fields + hit.fns;
                let msg = format!(
                    "{}: {} fields + {} methods = {total} (warn threshold {})",
                    hit.name, hit.fields, hit.fns, cfg.warn
                );
                Violation::warn(ID, hit.key, msg)
            })
            .collect();
        Ok(violations)
    }
}

/// One file's contribution: structs defined here (`name -> field count`) and
/// methods found here (`name -> count`, std-trait/test impls already filtered).
struct FileScan {
    rel: String,
    structs: BTreeMap<String, usize>,
    impl_fns: BTreeMap<String, usize>,
}

/// A flagged type after cross-file aggregation. `key` is `<def-file>::<name>`.
struct GodHit {
    key: String,
    name: String,
    fields: usize,
    fns: usize,
}

#[derive(Default)]
struct CrateScan {
    /// `name -> (file where the struct is defined, field count)`.
    structs: BTreeMap<String, (String, usize)>,
    /// `name -> methods summed across this crate's files`.
    impl_fns: BTreeMap<String, usize>,
}

/// Aggregate a type's `fields + methods` across every file of its owning crate,
/// so a god type whose `impl` blocks are spread over sibling files is counted
/// once by its true surface — and moving methods between files cannot lower the
/// count. Scoped per crate (`crates/<name>`), never workspace-wide, so two
/// same-named types in different crates stay separate.
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
        for (name, n) in scan.impl_fns {
            *entry.impl_fns.entry(name).or_insert(0) += n;
        }
    }

    let mut hits = Vec::new();
    for entry in crates.values() {
        for (name, (def_rel, fields)) in &entry.structs {
            let fns = entry.impl_fns.get(name).copied().unwrap_or(0);
            if fields + fns >= warn {
                hits.push(GodHit {
                    key: format!("{def_rel}::{name}"),
                    name: name.clone(),
                    fields: *fields,
                    fns,
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
    impl_fns: &mut BTreeMap<String, usize>,
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
                    let fns = im
                        .items
                        .iter()
                        .filter(
                            |it| matches!(it, ImplItem::Fn(f) if !attrs_have_cfg_test(&f.attrs)),
                        )
                        .count();
                    *impl_fns.entry(name).or_insert(0) += fns;
                }
            }
            Item::Mod(m) if attrs_have_cfg_test(&m.attrs) => {}
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect(inner, std_traits, structs, impl_fns);
                }
            }
            _ => {}
        }
    }
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

    fn counts(src: &str, name: &str) -> (usize, usize) {
        let std_traits: HashSet<&str> = ["Drop", "Default", "Debug", "From", "Add", "Ord"]
            .into_iter()
            .collect();
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let mut structs = BTreeMap::new();
        let mut impl_fns = BTreeMap::new();
        collect(&file.items, &std_traits, &mut structs, &mut impl_fns);
        (
            structs.get(name).copied().unwrap_or(0),
            impl_fns.get(name).copied().unwrap_or(0),
        )
    }

    #[test]
    fn production_fields_and_methods_are_counted() {
        let src = "struct S { a: u8, b: u8 }\n\
                   impl S { fn one(&self) {} fn two(&self) {} }\n";
        assert_eq!(counts(src, "S"), (2, 2));
    }

    #[test]
    fn cfg_test_impl_and_methods_are_excluded() {
        let src = "struct S { a: u8 }\n\
                   impl S { fn prod(&self) {} #[cfg(test)] fn helper(&self) {} }\n\
                   #[cfg(test)]\n\
                   impl S { fn t1(&self) {} fn t2(&self) {} }\n";
        // Only the one production method counts; the cfg(test) method and the
        // whole cfg(test) impl block are skipped.
        assert_eq!(counts(src, "S"), (1, 1));
    }

    #[test]
    fn std_trait_methods_are_excluded_domain_trait_methods_counted() {
        let src = "struct S { a: u8 }\n\
                   impl S { fn prod(&self) {} }\n\
                   impl std::ops::Add for S { type Output = S; fn add(self, o: S) -> S { o } }\n\
                   impl Drop for S { fn drop(&mut self) {} }\n\
                   impl Decoder for S { fn decode(&self) {} fn reset(&self) {} }\n";
        // 1 inherent + 2 domain-trait (Decoder); Add and Drop are std plumbing.
        assert_eq!(counts(src, "S"), (1, 3));
    }

    #[test]
    fn cfg_test_module_contents_are_excluded() {
        let src = "struct S { a: u8 }\n\
                   impl S { fn prod(&self) {} }\n\
                   #[cfg(test)]\n\
                   mod tests { impl super::S { fn t(&self) {} } }\n";
        assert_eq!(counts(src, "S"), (1, 1));
    }

    fn scan(rel: &str, structs: &[(&str, usize)], fns: &[(&str, usize)]) -> FileScan {
        FileScan {
            rel: rel.to_string(),
            structs: structs
                .iter()
                .map(|(n, c)| ((*n).to_string(), *c))
                .collect(),
            impl_fns: fns.iter().map(|(n, c)| ((*n).to_string(), *c)).collect(),
        }
    }

    fn hit(hits: &[GodHit], key: &str) -> Option<(usize, usize)> {
        hits.iter()
            .find(|h| h.key == key)
            .map(|h| (h.fields, h.fns))
    }

    #[test]
    fn methods_aggregate_across_a_crates_files() {
        // Type defined in state.rs, impls split across state.rs + access.rs.
        let scans = vec![
            scan(
                "crates/kithara-queue/src/queue/state.rs",
                &[("Queue", 6)],
                &[("Queue", 4)],
            ),
            scan(
                "crates/kithara-queue/src/queue/access.rs",
                &[],
                &[("Queue", 8)],
            ),
        ];
        let hits = aggregate(scans, 15);
        // 6 fields + (4 + 8) methods = 18, keyed by the struct-def file, once.
        assert_eq!(
            hit(&hits, "crates/kithara-queue/src/queue/state.rs::Queue"),
            Some((6, 12))
        );
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn moving_a_method_between_files_does_not_change_the_count() {
        let before = aggregate(
            vec![
                scan("crates/x/src/a.rs", &[("G", 3)], &[("G", 9)]),
                scan("crates/x/src/b.rs", &[], &[("G", 4)]),
            ],
            15,
        );
        let after = aggregate(
            vec![
                scan("crates/x/src/a.rs", &[("G", 3)], &[("G", 5)]),
                scan("crates/x/src/b.rs", &[], &[("G", 8)]),
            ],
            15,
        );
        assert_eq!(hit(&before, "crates/x/src/a.rs::G"), Some((3, 13)));
        assert_eq!(hit(&after, "crates/x/src/a.rs::G"), Some((3, 13)));
    }

    #[test]
    fn same_name_in_different_crates_stays_separate() {
        let scans = vec![
            scan("crates/a/src/lib.rs", &[("Config", 20)], &[("Config", 0)]),
            scan("crates/b/src/lib.rs", &[("Config", 2)], &[("Config", 3)]),
        ];
        let hits = aggregate(scans, 15);
        // a::Config crosses the threshold; b::Config (5) does not — no merge.
        assert_eq!(hit(&hits, "crates/a/src/lib.rs::Config"), Some((20, 0)));
        assert_eq!(hit(&hits, "crates/b/src/lib.rs::Config"), None);
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
