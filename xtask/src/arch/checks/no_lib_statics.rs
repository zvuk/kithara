use std::collections::BTreeSet;

use anyhow::Result;
use kithara_xtask_core::common::{
    parse::parse_file,
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};
use syn::{Item, ItemMacro, Type};

use super::{Check, Context};

pub(crate) const ID: &str = "no_lib_statics";

pub(crate) struct NoLibStatics;

impl Check for NoLibStatics {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.no_lib_statics;
        let exempt: BTreeSet<&str> = cfg.exempt_crates.iter().map(String::as_str).collect();
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path);
            let key = rel.to_string_lossy().replace('\\', "/");
            if is_main_or_bin(rel) || crate_is_exempt(rel, &exempt) {
                continue;
            }
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let mut hits: Vec<String> = Vec::new();
            walk(&file.items, &mut hits);

            for label in hits {
                violations.push(Violation::warn(
                    ID,
                    format!("{key}::{label}"),
                    format!(
                        "{label}: hidden global in library code; pass the singleton \
                         from the top of the app via config / constructor"
                    ),
                ));
            }
        }
        Ok(violations)
    }
}

fn walk(items: &[Item], out: &mut Vec<String>) {
    for item in items {
        match item {
            Item::Static(s) => {
                out.push(format!("static {}", s.ident));
            }
            Item::Const(c) if type_is_global_holder(&c.ty) => {
                out.push(format!("const {}", c.ident));
            }
            Item::Macro(m) if is_lazy_static_macro(m) => {
                out.push(macro_label(m));
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    walk(inner, out);
                }
            }
            _ => {}
        }
    }
}

fn is_main_or_bin(rel: &std::path::Path) -> bool {
    rel.file_name().and_then(|f| f.to_str()) == Some("main.rs")
}

fn crate_is_exempt(rel: &std::path::Path, exempt: &BTreeSet<&str>) -> bool {
    let mut comps = rel.components();
    if comps.next().and_then(|c| c.as_os_str().to_str()) != Some("crates") {
        return true;
    }
    let Some(crate_name) = comps
        .next()
        .and_then(|c| c.as_os_str().to_str().map(String::from))
    else {
        return true;
    };
    exempt.contains(crate_name.as_str())
}

fn type_is_global_holder(ty: &Type) -> bool {
    let Type::Path(p) = ty else { return false };
    p.path.segments.iter().any(|s| {
        matches!(
            s.ident.to_string().as_str(),
            "OnceLock" | "OnceCell" | "LazyLock" | "Lazy"
        )
    })
}

fn is_lazy_static_macro(m: &ItemMacro) -> bool {
    let path = &m.mac.path;
    path.segments
        .last()
        .is_some_and(|s| s.ident == "lazy_static")
}

fn macro_label(m: &ItemMacro) -> String {
    let name = m
        .mac
        .path
        .segments
        .last()
        .map_or_else(|| "?".to_string(), |s| s.ident.to_string());
    format!("{name}!{{...}}")
}
