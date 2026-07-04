use std::collections::BTreeMap;

use anyhow::Result;
use kithara_xtask_core::common::{
    parse::{is_strict_pub, parse_file},
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};
use syn::{Fields, Item, ItemStruct};

use super::{Check, Context};

pub(crate) const ID: &str = "pub_struct_open_fields";

pub(crate) struct PubStructOpenFields;

impl Check for PubStructOpenFields {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.pub_struct_open_fields;
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path)
                .to_string_lossy()
                .replace('\\', "/");

            let mut hits: BTreeMap<String, usize> = BTreeMap::new();
            collect(&file.items, &mut hits);

            for (name, n) in &hits {
                if *n >= cfg.warn {
                    let key = format!("{rel}::{name}");
                    let msg = format!(
                        "{name}: {n} pub fields on a pub struct (warn threshold {}); \
                         consider a builder, encapsulated setters, or grouping into \
                         a typed value",
                        cfg.warn
                    );
                    violations.push(Violation::warn(ID, key, msg));
                }
            }
        }
        Ok(violations)
    }
}

fn collect(items: &[Item], out: &mut BTreeMap<String, usize>) {
    for item in items {
        match item {
            Item::Struct(s) if is_strict_pub(&s.vis) => {
                let n = count_pub_fields(s);
                if n > 0 {
                    out.insert(s.ident.to_string(), n);
                }
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect(inner, out);
                }
            }
            _ => {}
        }
    }
}

fn count_pub_fields(s: &ItemStruct) -> usize {
    match &s.fields {
        Fields::Named(n) => n.named.iter().filter(|f| is_strict_pub(&f.vis)).count(),
        Fields::Unnamed(u) => u.unnamed.iter().filter(|f| is_strict_pub(&f.vis)).count(),
        Fields::Unit => 0,
    }
}
