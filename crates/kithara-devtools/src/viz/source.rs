use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::Result;
use cargo_metadata::{DependencyKind, Package, Target, TargetKind};
use syn::Item;

use super::graph::{Edge, EdgeKind, Evidence, EvidenceGraph, Node, NodeId, NodeKind};
use crate::{
    Ctx,
    common::{
        parse::{is_pub_visibility, parse_file},
        walker::{relative_to, walk_rs_files},
    },
};

pub(crate) fn collect(ctx: &Ctx) -> Result<EvidenceGraph> {
    let metadata = ctx.metadata()?;
    let mut packages = metadata
        .workspace_packages()
        .into_iter()
        .collect::<Vec<_>>();
    packages.sort_by(|left, right| left.name.cmp(&right.name));

    let mut graph = EvidenceGraph::default();
    for package in &packages {
        collect_package(ctx, package, &mut graph)?;
    }
    collect_dependencies(&packages, &mut graph);
    Ok(graph)
}

fn collect_dependencies(packages: &[&Package], graph: &mut EvidenceGraph) {
    let paths = packages
        .iter()
        .filter_map(|package| {
            package
                .manifest_path
                .parent()
                .map(|path| (path, package.name.as_str()))
        })
        .collect::<BTreeMap<_, _>>();

    for package in packages {
        for dependency in &package.dependencies {
            if dependency.kind != DependencyKind::Normal {
                continue;
            }
            let Some(target) = dependency.path.as_deref().and_then(|path| paths.get(path)) else {
                continue;
            };
            let source = package.name.as_str();
            let evidence = if dependency.optional || dependency.target.is_some() {
                let target_condition = dependency
                    .target
                    .as_ref()
                    .map_or_else(|| "any-target".to_string(), ToString::to_string);
                Evidence::conditional(format!(
                    "cargo:{source}->{target}:optional={}:target={target_condition}",
                    dependency.optional
                ))
            } else {
                Evidence::resolved(format!("cargo:{source}->{target}"))
            };
            graph.merge_edge(Edge::new(
                NodeId::package(source),
                NodeId::package(target),
                EdgeKind::DependsOn,
                evidence,
            ));
        }
    }
}

fn collect_package(ctx: &Ctx, package: &Package, graph: &mut EvidenceGraph) -> Result<()> {
    let package_name = package.name.to_string();
    let package_id = NodeId::package(&package_name);
    graph.merge_node(Node::new(
        package_id.clone(),
        NodeKind::Package,
        &package_name,
        Evidence::resolved(format!("package:{package_name}")),
    ));

    let mut targets = package
        .targets
        .iter()
        .filter(|target| is_production_target(target))
        .collect::<Vec<_>>();
    targets.sort_by_key(|target| (target_rank(target), target.name.as_str()));
    if let Some(target) = targets.first() {
        collect_target(ctx, &package_name, &package_id, target, graph)?;
    }
    Ok(())
}

fn is_production_target(target: &Target) -> bool {
    target.kind.iter().any(|kind| {
        matches!(
            kind,
            TargetKind::Bin
                | TargetKind::CDyLib
                | TargetKind::DyLib
                | TargetKind::Lib
                | TargetKind::ProcMacro
                | TargetKind::RLib
                | TargetKind::StaticLib
        )
    })
}

fn target_rank(target: &Target) -> u8 {
    if target.kind.iter().any(|kind| {
        matches!(
            kind,
            TargetKind::Lib | TargetKind::ProcMacro | TargetKind::RLib
        )
    }) {
        0
    } else {
        1
    }
}

fn collect_target(
    ctx: &Ctx,
    package: &str,
    package_id: &NodeId,
    target: &Target,
    graph: &mut EvidenceGraph,
) -> Result<()> {
    let root_file = target.src_path.as_std_path();
    let module_root = module_root(root_file);
    let mut files = BTreeSet::new();
    if module_root.is_dir() {
        files.extend(walk_rs_files(&module_root)?);
    }
    files.remove(root_file);

    let mut parsed = Vec::new();
    for path in std::iter::once(root_file.to_path_buf()).chain(files) {
        let Ok(file) = parse_file(&path) else {
            continue;
        };
        let module = module_path(root_file, &module_root, &path);
        let relative = relative_to(&ctx.root, &path)
            .to_string_lossy()
            .replace('\\', "/");
        parsed.push((path, module, relative, file));
    }
    let test_modules = parsed
        .iter()
        .flat_map(|(_, module, _, file)| {
            file.items.iter().filter_map(move |item| {
                let Item::Mod(item) = item else {
                    return None;
                };
                (item.content.is_none() && super::calls::is_test_only(&item.attrs))
                    .then(|| child_module(module, &item.ident.to_string()))
            })
        })
        .collect::<BTreeSet<_>>();
    parsed.retain(|(_, module, _, _)| {
        !test_modules
            .iter()
            .any(|test_module| module_within(module, test_module))
    });

    for (_, module, relative, file) in &parsed {
        ensure_module_chain(package, &target.name, package_id, module, relative, graph);
        collect_public_items(package, &target.name, module, relative, &file.items, graph);
        super::ownership::collect(package, &target.name, module, relative, file, graph);
    }

    let mut call_index = super::calls::CallIndex::default();
    for (_, module, relative, file) in &parsed {
        super::calls::collect_abstractions(
            &super::calls::SourceUnit {
                package,
                target: &target.name,
                module,
                relative,
                file,
            },
            graph,
            &mut call_index,
        );
    }
    for (_, module, relative, file) in &parsed {
        super::calls::collect_definitions(
            &super::calls::SourceUnit {
                package,
                target: &target.name,
                module,
                relative,
                file,
            },
            graph,
            &mut call_index,
        );
    }
    for (_, module, relative, file) in &parsed {
        super::calls::collect_edges(
            &super::calls::SourceUnit {
                package,
                target: &target.name,
                module,
                relative,
                file,
            },
            graph,
            &call_index,
        );
    }
    Ok(())
}

fn child_module(parent: &str, child: &str) -> String {
    if parent == "crate" {
        child.to_string()
    } else {
        format!("{parent}::{child}")
    }
}

fn module_within(module: &str, parent: &str) -> bool {
    module == parent
        || module
            .strip_prefix(parent)
            .is_some_and(|suffix| suffix.starts_with("::"))
}

fn module_root(root_file: &Path) -> PathBuf {
    let parent = root_file.parent().unwrap_or(root_file);
    match root_file.file_stem().and_then(|stem| stem.to_str()) {
        Some("lib" | "main" | "mod") | None => parent.to_path_buf(),
        Some(stem) => parent.join(stem),
    }
}

fn module_path(root_file: &Path, module_root: &Path, path: &Path) -> String {
    if path == root_file {
        return "crate".to_string();
    }
    let relative = relative_to(module_root, path);
    let mut parts = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    if let Some(last) = parts.last_mut() {
        if last == "mod.rs" {
            parts.pop();
        } else if let Some(stem) = last.strip_suffix(".rs") {
            *last = stem.to_string();
        }
    }
    if parts.is_empty() {
        "crate".to_string()
    } else {
        parts.join("::")
    }
}

fn ensure_module_chain(
    package: &str,
    target: &str,
    package_id: &NodeId,
    module: &str,
    relative: &str,
    graph: &mut EvidenceGraph,
) {
    let parts = if module == "crate" {
        Vec::new()
    } else {
        module.split("::").collect::<Vec<_>>()
    };
    let mut parent = package_id.clone();
    for depth in 0..=parts.len() {
        let current = if depth == 0 {
            "crate".to_string()
        } else {
            parts[..depth].join("::")
        };
        let id = NodeId::module(package, target, &current);
        let label = parts.get(depth.wrapping_sub(1)).copied().unwrap_or("crate");
        let module_evidence = if current == "crate" {
            Evidence::static_fact(format!("{relative}:module:{current}"))
        } else {
            Evidence::inferred(format!("{relative}:module:{current}"))
        };
        graph.merge_node(
            Node::new(id.clone(), NodeKind::Module, label, module_evidence).at(relative, 1),
        );
        let containment_evidence = if current == "crate" {
            Evidence::resolved(format!("{relative}:contains:{current}"))
        } else {
            Evidence::inferred(format!("{relative}:contains:{current}"))
        };
        graph.merge_edge(Edge::new(
            parent,
            id.clone(),
            EdgeKind::Contains,
            containment_evidence,
        ));
        parent = id;
    }
}

fn collect_public_items(
    package: &str,
    target: &str,
    module: &str,
    relative: &str,
    items: &[Item],
    graph: &mut EvidenceGraph,
) {
    let module_id = NodeId::module(package, target, module);
    for item in items {
        let Some((symbol, label, line)) = public_item(item) else {
            continue;
        };
        let id = NodeId::symbol(package, target, module, &symbol);
        let origin = format!("{relative}:{line}");
        graph.merge_node(
            Node::new(
                id.clone(),
                NodeKind::PublicItem,
                label,
                Evidence::static_fact(origin.clone()),
            )
            .at(relative, line),
        );
        graph.merge_edge(Edge::new(
            module_id.clone(),
            id,
            EdgeKind::Contains,
            Evidence::static_fact(origin),
        ));
    }
}

fn public_item(item: &Item) -> Option<(String, String, usize)> {
    use syn::spanned::Spanned as _;

    match item {
        Item::Struct(item)
            if is_pub_visibility(&item.vis) && !super::calls::is_test_only(&item.attrs) =>
        {
            Some((
                item.ident.to_string(),
                format!("struct {}", item.ident),
                item.span().start().line,
            ))
        }
        Item::Enum(item)
            if is_pub_visibility(&item.vis) && !super::calls::is_test_only(&item.attrs) =>
        {
            Some((
                item.ident.to_string(),
                format!("enum {}", item.ident),
                item.span().start().line,
            ))
        }
        Item::Trait(item)
            if is_pub_visibility(&item.vis) && !super::calls::is_test_only(&item.attrs) =>
        {
            Some((
                item.ident.to_string(),
                format!("trait {}", item.ident),
                item.span().start().line,
            ))
        }
        Item::Type(item)
            if is_pub_visibility(&item.vis) && !super::calls::is_test_only(&item.attrs) =>
        {
            Some((
                item.ident.to_string(),
                format!("type {}", item.ident),
                item.span().start().line,
            ))
        }
        Item::Const(item)
            if is_pub_visibility(&item.vis) && !super::calls::is_test_only(&item.attrs) =>
        {
            Some((
                item.ident.to_string(),
                format!("const {}", item.ident),
                item.span().start().line,
            ))
        }
        Item::Static(item)
            if is_pub_visibility(&item.vis) && !super::calls::is_test_only(&item.attrs) =>
        {
            Some((
                item.ident.to_string(),
                format!("static {}", item.ident),
                item.span().start().line,
            ))
        }
        _ => None,
    }
}
