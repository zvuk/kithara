//! `cargo xtask viz` — visual maps of architecture problems detected by
//! the lint suite. Two read-only subcommands:
//!
//!   * `viz hierarchy [<crate>]` — tree of modules and the public items
//!     they declare. Pairs with `god_module` / `mixed_entities` /
//!     `pub_struct_open_fields` warnings to make crate-surface obvious.
//!   * `viz arc-map [<crate>]` — every site that creates, clones, or
//!     holds an `Arc<T>`. Pairs with `shared_state` / `arc_clone_hotspots`
//!     warnings to make the ownership-distribution graph visible.
//!
//! Output is plain text, sorted, scannable in a terminal. No baseline,
//! no exit-code semantics — these are explorers, not gates.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;
use clap::{Args, Subcommand};
use syn::{Expr, Item, Type, visit::Visit};

use crate::common::{
    parse::{is_pub_visibility, parse_file},
    walker::{relative_to, walk_rs_files},
};

#[derive(Debug, Args)]
pub(crate) struct VizArgs {
    #[command(subcommand)]
    pub(crate) command: VizCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum VizCommand {
    /// Tree-view of modules + their declared `pub`/`pub(crate)` items.
    Hierarchy(HierarchyArgs),
    /// Map of `Arc<T>` creation, clone, and field-storage sites.
    ArcMap(ArcMapArgs),
}

#[derive(Debug, Args)]
pub(crate) struct HierarchyArgs {
    /// Single crate to inspect (e.g. `kithara-play`); empty = all crates.
    pub(crate) krate: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ArcMapArgs {
    /// Single crate to inspect (e.g. `kithara-play`); empty = all crates.
    pub(crate) krate: Option<String>,
}

pub(crate) fn run(args: &VizArgs) -> Result<()> {
    match &args.command {
        VizCommand::Hierarchy(a) => run_hierarchy(a),
        VizCommand::ArcMap(a) => run_arc_map(a),
    }
}

// --- hierarchy --------------------------------------------------------------

fn run_hierarchy(args: &HierarchyArgs) -> Result<()> {
    let workspace_root = workspace_root()?;
    for krate in select_crates(&workspace_root, args.krate.as_deref())? {
        println!("=== {} ===", krate.name);
        let mut entries = collect_module_entries(&krate.src_dir)?;
        entries.sort_by(|a, b| a.module_path.cmp(&b.module_path));
        for entry in &entries {
            let depth = entry.module_path.len();
            let indent = "  ".repeat(depth);
            let name = entry
                .module_path
                .last()
                .map_or("(crate root)", String::as_str);
            print!("{indent}{name}");
            if entry.pub_items.is_empty() {
                println!();
            } else {
                println!("  [{}]", entry.pub_items.len());
                for item in &entry.pub_items {
                    println!("{indent}  - {item}");
                }
            }
        }
        println!();
    }
    Ok(())
}

struct ModuleEntry {
    module_path: Vec<String>,
    pub_items: Vec<String>,
}

fn collect_module_entries(src_dir: &Path) -> Result<Vec<ModuleEntry>> {
    let mut out = Vec::new();
    for path in walk_rs_files(src_dir)? {
        let Ok(file) = parse_file(&path) else {
            continue;
        };
        let rel = relative_to(src_dir, &path);
        let module_path = rs_path_to_module_path(rel);
        let pub_items = list_pub_items(&file.items);
        out.push(ModuleEntry {
            module_path,
            pub_items,
        });
    }
    Ok(out)
}

fn rs_path_to_module_path(rel: &Path) -> Vec<String> {
    let mut out: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    if let Some(last) = out.last_mut() {
        if matches!(last.as_str(), "lib.rs" | "main.rs" | "mod.rs") {
            out.pop();
        } else if let Some(stem) = last.strip_suffix(".rs") {
            *last = stem.to_string();
        }
    }
    out
}

fn list_pub_items(items: &[Item]) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        match item {
            Item::Struct(s) if is_pub_visibility(&s.vis) => out.push(format!("struct {}", s.ident)),
            Item::Enum(e) if is_pub_visibility(&e.vis) => out.push(format!("enum {}", e.ident)),
            Item::Trait(t) if is_pub_visibility(&t.vis) => out.push(format!("trait {}", t.ident)),
            Item::Fn(f) if is_pub_visibility(&f.vis) => out.push(format!("fn {}()", f.sig.ident)),
            Item::Type(t) if is_pub_visibility(&t.vis) => out.push(format!("type {}", t.ident)),
            Item::Const(c) if is_pub_visibility(&c.vis) => out.push(format!("const {}", c.ident)),
            Item::Static(s) if is_pub_visibility(&s.vis) => out.push(format!("static {}", s.ident)),
            _ => {}
        }
    }
    out.sort();
    out
}

// --- arc-map ----------------------------------------------------------------

fn run_arc_map(args: &ArcMapArgs) -> Result<()> {
    let workspace_root = workspace_root()?;
    for krate in select_crates(&workspace_root, args.krate.as_deref())? {
        let report = build_arc_report(&krate)?;
        if report.is_empty() {
            continue;
        }
        println!("=== {} ===", krate.name);
        for (file, sites) in &report {
            println!("{file}");
            for s in sites {
                println!("  L{:<4}  {}  {}", s.line, s.kind.tag(), s.label);
            }
        }
        println!();
    }
    Ok(())
}

#[derive(Debug)]
struct ArcSite {
    line: usize,
    kind: ArcKind,
    label: String,
}

#[derive(Debug, Clone, Copy)]
enum ArcKind {
    Create,
    Clone,
    Field,
}

impl ArcKind {
    fn tag(self) -> &'static str {
        match self {
            Self::Create => "CREATE",
            Self::Clone => "CLONE ",
            Self::Field => "FIELD ",
        }
    }
}

fn build_arc_report(krate: &CrateLoc) -> Result<BTreeMap<String, Vec<ArcSite>>> {
    let mut out: BTreeMap<String, Vec<ArcSite>> = BTreeMap::new();
    for path in walk_rs_files(&krate.src_dir)? {
        let Ok(file) = parse_file(&path) else {
            continue;
        };
        let rel = relative_to(&krate.src_dir, &path);
        let key = format!("src/{}", rel.to_string_lossy().replace('\\', "/"));

        let mut sites: Vec<ArcSite> = Vec::new();
        for item in &file.items {
            collect_arc_fields(item, &mut sites);
        }
        let mut visitor = ArcCallVisitor { sites: &mut sites };
        visitor.visit_file(&file);

        if !sites.is_empty() {
            sites.sort_by_key(|s| (s.line, s.kind as u8));
            out.insert(key, sites);
        }
    }
    Ok(out)
}

fn collect_arc_fields(item: &Item, sites: &mut Vec<ArcSite>) {
    match item {
        Item::Struct(s) => {
            for field in &s.fields {
                if let Some(label) = arc_field_label(field) {
                    let line = syn::spanned::Spanned::span(field).start().line;
                    sites.push(ArcSite {
                        line,
                        kind: ArcKind::Field,
                        label: format!("{}::{label}", s.ident),
                    });
                }
            }
        }
        Item::Mod(m) => {
            if let Some((_, inner)) = &m.content {
                for it in inner {
                    collect_arc_fields(it, sites);
                }
            }
        }
        _ => {}
    }
}

fn arc_field_label(f: &syn::Field) -> Option<String> {
    let inner = arc_inner(&f.ty)?;
    let name = f
        .ident
        .as_ref()
        .map_or_else(|| "?".to_string(), ToString::to_string);
    Some(format!("{name}: Arc<{inner}>"))
}

fn arc_inner(ty: &Type) -> Option<String> {
    let Type::Path(p) = ty else { return None };
    let last = p.path.segments.last()?;
    if last.ident != "Arc" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };
    let inner_ty = args.args.first()?;
    Some(render_generic_arg(inner_ty))
}

fn render_generic_arg(arg: &syn::GenericArgument) -> String {
    match arg {
        syn::GenericArgument::Type(t) => render_type(t),
        _ => "?".to_string(),
    }
}

fn render_type(ty: &Type) -> String {
    match ty {
        Type::Path(p) => p
            .path
            .segments
            .iter()
            .map(|s| {
                let mut out = s.ident.to_string();
                if let syn::PathArguments::AngleBracketed(args) = &s.arguments {
                    let inner: Vec<String> = args.args.iter().map(render_generic_arg).collect();
                    out.push('<');
                    out.push_str(&inner.join(","));
                    out.push('>');
                }
                out
            })
            .collect::<Vec<_>>()
            .join("::"),
        Type::Reference(r) => format!("&{}", render_type(&r.elem)),
        Type::Tuple(t) => {
            let inner: Vec<String> = t.elems.iter().map(render_type).collect();
            format!("({})", inner.join(","))
        }
        _ => "?".to_string(),
    }
}

struct ArcCallVisitor<'a> {
    sites: &'a mut Vec<ArcSite>,
}

impl<'ast> Visit<'ast> for ArcCallVisitor<'_> {
    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        if let Expr::Path(p) = c.func.as_ref() {
            let segs: Vec<String> = p
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            let n = segs.len();
            if n >= 2 && segs[n - 2] == "Arc" {
                let line = syn::spanned::Spanned::span(&c.func).start().line;
                match segs[n - 1].as_str() {
                    "new" | "new_cyclic" | "from" => self.sites.push(ArcSite {
                        line,
                        kind: ArcKind::Create,
                        label: format!("Arc::{}(...)", segs[n - 1]),
                    }),
                    "clone" => self.sites.push(ArcSite {
                        line,
                        kind: ArcKind::Clone,
                        label: "Arc::clone(...)".to_string(),
                    }),
                    _ => {}
                }
            }
        }
        syn::visit::visit_expr_call(self, c);
    }
}

// --- shared infra -----------------------------------------------------------

struct CrateLoc {
    name: String,
    src_dir: PathBuf,
}

fn workspace_root() -> Result<PathBuf> {
    let metadata = MetadataCommand::new().no_deps().exec()?;
    Ok(metadata.workspace_root.as_std_path().to_path_buf())
}

fn select_crates(workspace_root: &Path, name_filter: Option<&str>) -> Result<Vec<CrateLoc>> {
    let crates_dir = workspace_root.join("crates");
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&crates_dir)
        .with_context(|| format!("read crates dir: {}", crates_dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(want) = name_filter
            && name != want
        {
            continue;
        }
        let src_dir = path.join("src");
        if src_dir.exists() {
            out.push(CrateLoc { name, src_dir });
        }
    }
    if let Some(want) = name_filter
        && out.is_empty()
    {
        anyhow::bail!("crate not found: {want}");
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}
