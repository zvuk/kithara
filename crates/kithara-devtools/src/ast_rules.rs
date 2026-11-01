//! Resolve the caller's native ast-grep configuration and source scope.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};
use glob::Pattern;
use serde::Deserialize;
use serde_yaml_ng::{Deserializer, Value};

use crate::{
    Ctx,
    common::{
        exclude::{cfg_test_module_globs, cfg_test_module_globs_for_targets},
        scope::Scope,
        walker::{matches_any, workspace_rs_files_scoped},
    },
};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get, deref = false)]
pub(crate) struct Rules {
    catalog: BTreeMap<String, BTreeSet<String>>,
    #[field(get(deref = Path), vis = "pub(crate)")]
    config: PathBuf,
}

impl Rules {
    pub(crate) fn load(ctx: &Ctx) -> Result<Self> {
        let mut catalog = BTreeMap::new();
        let mut identities = BTreeSet::new();
        let config = ctx
            .root
            .join("sgconfig.yml")
            .canonicalize()
            .context("resolve caller ast-grep configuration")?;
        let text = fs::read_to_string(&config)
            .with_context(|| format!("read ast-grep configuration {}", config.display()))?;
        let value: Value = serde_yaml_ng::from_str(&text)
            .with_context(|| format!("parse {}", config.display()))?;
        let dirs = value
            .get("ruleDirs")
            .and_then(Value::as_sequence)
            .context("ast-grep ruleDirs must be a list")?;
        for entry in dirs {
            let relative = entry
                .as_str()
                .context("ast-grep ruleDirs must contain paths")?;
            index_directory(&ctx.root.join(relative), &mut catalog, &mut identities)?;
        }
        for id in &ctx.config.lint_exclude.scan_all_rules {
            ensure!(
                catalog.contains_key(id),
                "unknown scan-all ast-grep rule `{id}`"
            );
        }
        Ok(Self { catalog, config })
    }

    pub(crate) fn rule(&self, id: &str) -> Result<&BTreeSet<String>> {
        self.catalog
            .get(id)
            .with_context(|| format!("unknown ast-grep rule `{id}`"))
    }
}

fn index_directory(
    directory: &Path,
    catalog: &mut BTreeMap<String, BTreeSet<String>>,
    identities: &mut BTreeSet<String>,
) -> Result<()> {
    let mut entries = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    for path in entries {
        let file_type = fs::symlink_metadata(&path)?.file_type();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            index_directory(&path, catalog, identities)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "yml" || extension == "yaml")
        {
            index_file(&path, catalog, identities)?;
        }
    }
    Ok(())
}

fn index_file(
    path: &Path,
    catalog: &mut BTreeMap<String, BTreeSet<String>>,
    identities: &mut BTreeSet<String>,
) -> Result<()> {
    let text = fs::read_to_string(path)?;
    for document in Deserializer::from_str(&text) {
        let rule = Value::deserialize(document)
            .with_context(|| format!("parse ast-grep rule {}", path.display()))?;
        let id = rule
            .get("id")
            .and_then(Value::as_str)
            .with_context(|| format!("ast-grep rule {} has no identity", path.display()))?;
        ensure!(
            identities.insert(id.to_owned()),
            "duplicate ast-grep rule `{id}`"
        );
        catalog
            .entry(id.to_owned())
            .or_default()
            .insert(id.to_owned());
        if let Some(category) = rule
            .get("metadata")
            .and_then(|metadata| metadata.get("category"))
            .and_then(Value::as_str)
            && category != id
        {
            catalog
                .entry(category.to_owned())
                .or_default()
                .insert(id.to_owned());
        }
    }
    Ok(())
}

pub(crate) fn source_paths(ctx: &Ctx, paths: &[String]) -> Result<Vec<PathBuf>> {
    if !paths.is_empty() {
        return Ok(paths.iter().map(PathBuf::from).collect());
    }
    if !ctx.config.ast_grep.default_roots.is_empty() {
        return Ok(ctx.config.ast_grep.default_roots.clone());
    }
    let Ok(metadata) = ctx.metadata() else {
        return Ok(vec![PathBuf::from(".")]);
    };
    let scope = Scope::default().with_metadata(metadata);
    Ok(workspace_rs_files_scoped(&ctx.root, &scope)?
        .into_iter()
        .map(|path| path.strip_prefix(&ctx.root).unwrap_or(&path).to_path_buf())
        .collect())
}

pub(crate) fn excluded_globs(ctx: &Ctx, include_tests: bool) -> Result<Vec<Pattern>> {
    let mut paths = ctx.config.workspace_scan.exclude.clone();
    if !include_tests {
        paths.extend(ctx.config.lint_exclude.runtime_paths());
        paths.extend(test_module_globs(ctx));
    }
    paths
        .iter()
        .map(|path| {
            Pattern::new(path).with_context(|| format!("invalid ast-grep exclusion `{path}`"))
        })
        .collect()
}

/// Cargo-backed contexts use declared targets; standalone contexts use their source graph.
pub(crate) fn test_module_globs(ctx: &Ctx) -> Vec<String> {
    ctx.metadata().map_or_else(
        |_| cfg_test_module_globs(&ctx.root),
        |metadata| {
            cfg_test_module_globs_for_targets(
                &ctx.root,
                metadata
                    .workspace_packages()
                    .into_iter()
                    .flat_map(|package| {
                        package
                            .targets
                            .iter()
                            .map(|target| target.src_path.as_std_path())
                    }),
            )
        },
    )
}

pub(crate) fn filter_file_paths(
    ctx: &Ctx,
    paths: &[PathBuf],
    include_tests: bool,
) -> Result<Vec<PathBuf>> {
    let excludes = excluded_globs(ctx, include_tests)?;
    Ok(paths
        .iter()
        .filter(|path| {
            let relative = path.strip_prefix(&ctx.root).unwrap_or(path);
            !ctx.root.join(path).is_file() || !matches_any(&excludes, relative)
        })
        .cloned()
        .collect())
}
