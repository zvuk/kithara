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
        let mut violations = Vec::new();

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

            for (name, fields) in &structs {
                let fns = impl_fns.get(name).copied().unwrap_or(0);
                let total = fields + fns;
                if total >= cfg.warn {
                    let key = format!("{rel}::{name}");
                    let msg = format!(
                        "{name}: {fields} fields + {fns} methods = {total} (warn threshold {})",
                        cfg.warn
                    );
                    violations.push(Violation::warn(ID, key, msg));
                }
            }
        }
        Ok(violations)
    }
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
}
