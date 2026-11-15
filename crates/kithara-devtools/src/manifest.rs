use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, cargo_platform::Platform};
use clap::{Args, Subcommand};
use toml::Value;

use crate::{Ctx, util::ensure_clean_tree};

const HAKARI_SECTION_START: &str = "### BEGIN HAKARI SECTION";
const HAKARI_SECTION_END: &str = "### END HAKARI SECTION";

#[derive(Debug, Args)]
pub struct ManifestArgs {
    #[command(subcommand)]
    pub command: ManifestCommand,
}

#[derive(Debug, Subcommand)]
pub enum ManifestCommand {
    /// Keep workspace dependencies before external dependencies.
    DependencyOrder(DependencyOrderArgs),
}

#[derive(Debug, Args)]
pub struct DependencyOrderArgs {
    /// Allow `--fix` on a dirty working tree.
    #[arg(long = "allow-dirty")]
    pub allow_dirty: bool,
    /// Rewrite manifests in place.
    #[arg(long)]
    pub fix: bool,
}

struct ManifestSet {
    workspace_root: PathBuf,
    manifests: Vec<PathBuf>,
}

#[derive(Debug, Eq, PartialEq)]
struct RewriteResult {
    content: String,
    changed_sections: Vec<String>,
}

#[derive(Debug)]
struct DependencyBlock<'a> {
    key: &'a str,
    lines: Vec<&'a str>,
}

pub(crate) fn run(args: &ManifestArgs, ctx: &Ctx) -> Result<()> {
    match &args.command {
        ManifestCommand::DependencyOrder(args) => dependency_order(args, ctx),
    }
}

fn dependency_order(args: &DependencyOrderArgs, ctx: &Ctx) -> Result<()> {
    if args.fix {
        ensure_clean_tree(&ctx.root, args.allow_dirty, "manifest dependency-order")?;
    }

    let metadata = ctx.metadata()?;
    let manifest_set = workspace_manifests(metadata);
    let mut changed = Vec::new();

    for manifest in &manifest_set.manifests {
        let original =
            fs::read_to_string(manifest).with_context(|| format!("read {}", manifest.display()))?;
        let internal = internal_dependencies(metadata, manifest, &original)?;
        let rewritten = rewrite_manifest(&original, &internal);
        if original == rewritten.content {
            continue;
        }

        let display_path = display_path(&manifest_set.workspace_root, manifest);
        changed.push(format!(
            "{}: {}",
            display_path,
            rewritten.changed_sections.join(", ")
        ));

        if args.fix {
            fs::write(manifest, rewritten.content)
                .with_context(|| format!("write {}", manifest.display()))?;
        }
    }

    if changed.is_empty() {
        return Ok(());
    }

    if args.fix {
        println!(
            "normalized internal dependency order in {} manifest(s)",
            changed.len()
        );
        for item in changed {
            println!("  {item}");
        }
        return Ok(());
    }

    bail!(
        "Cargo manifests must list workspace dependencies before external dependencies and keep each dependency group sorted:\n{}",
        changed.join("\n")
    )
}

fn workspace_manifests(metadata: &Metadata) -> ManifestSet {
    let workspace_root = metadata.workspace_root.as_std_path().to_path_buf();
    let mut manifests = BTreeSet::new();

    manifests.insert(workspace_root.join("Cargo.toml"));
    for package in metadata.workspace_packages() {
        manifests.insert(package.manifest_path.as_std_path().to_path_buf());
    }

    ManifestSet {
        workspace_root,
        manifests: manifests.into_iter().collect(),
    }
}

pub(crate) fn handwritten_workspace_manifest_dirs() -> Result<Vec<PathBuf>> {
    let metadata = MetadataCommand::new()
        .no_deps()
        .exec()
        .context("load cargo metadata")?;
    handwritten_manifest_dirs(&workspace_manifests(&metadata).manifests)
}

fn handwritten_manifest_dirs(manifests: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    for manifest in manifests {
        let source =
            fs::read_to_string(manifest).with_context(|| format!("read {}", manifest.display()))?;
        if source
            .lines()
            .any(|line| line.trim() == HAKARI_SECTION_START)
        {
            continue;
        }
        let directory = manifest
            .parent()
            .with_context(|| format!("manifest {} has no directory", manifest.display()))?;
        directories.push(directory.to_path_buf());
    }
    Ok(directories)
}

fn display_path(workspace_root: &Path, path: &Path) -> String {
    path.strip_prefix(workspace_root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn rewrite_manifest(source: &str, internal: &BTreeMap<String, BTreeSet<String>>) -> RewriteResult {
    let lines: Vec<_> = source.split_inclusive('\n').collect();
    let mut output = String::with_capacity(source.len());
    let mut changed_sections = Vec::new();
    let mut index = 0;
    let mut generated = false;

    while index < lines.len() {
        match lines[index].trim() {
            HAKARI_SECTION_START => generated = true,
            HAKARI_SECTION_END => generated = false,
            _ => {}
        }
        if generated {
            output.push_str(lines[index]);
            index += 1;
            continue;
        }
        let Some(header) = table_header(lines[index]) else {
            output.push_str(lines[index]);
            index += 1;
            continue;
        };

        output.push_str(lines[index]);
        let body_start = index + 1;
        let body_end = section_body_end(&lines, body_start);

        if is_dependency_section(header) {
            let body = rewrite_dependency_body(&lines[body_start..body_end], internal.get(header));
            if body.changed {
                changed_sections.push(header.to_owned());
            }
            output.push_str(&body.content);
        } else {
            for line in &lines[body_start..body_end] {
                output.push_str(line);
            }
        }

        index = body_end;
    }

    RewriteResult {
        changed_sections,
        content: output,
    }
}

pub(crate) fn add_workspace_dependency(
    source: &str,
    dependency: &str,
    manifest: &Path,
    metadata: &Metadata,
) -> Result<Option<String>> {
    let parsed: Value = toml::from_str(source).context("parse Cargo manifest")?;
    if parsed
        .get("dependencies")
        .and_then(Value::as_table)
        .is_some_and(|dependencies| dependencies.contains_key(dependency))
    {
        return Ok(None);
    }

    let lines: Vec<_> = source.split_inclusive('\n').collect();
    let mut offset = 0;
    for line in lines {
        offset += line.len();
        if table_header(line) != Some("dependencies") {
            continue;
        }
        let mut inserted = String::with_capacity(source.len() + dependency.len() + 32);
        inserted.push_str(&source[..offset]);
        if !inserted.ends_with('\n') {
            inserted.push('\n');
        }
        inserted.push_str(dependency);
        inserted.push_str(" = { workspace = true }\n");
        inserted.push_str(&source[offset..]);
        let internal = internal_dependencies(metadata, manifest, &inserted)?;
        return Ok(Some(rewrite_manifest(&inserted, &internal).content));
    }

    bail!("Cargo manifest has no [dependencies] section")
}

fn section_body_end(lines: &[&str], body_start: usize) -> usize {
    lines[body_start..]
        .iter()
        .position(|line| {
            table_header(line).is_some()
                || matches!(line.trim(), HAKARI_SECTION_START | HAKARI_SECTION_END)
        })
        .map_or(lines.len(), |offset| body_start + offset)
}

fn table_header(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.starts_with("[[") {
        let end = trimmed.find("]]")?;
        let rest = trimmed[end + 2..].trim_start();
        if rest.is_empty() || rest.starts_with('#') {
            return Some(trimmed[2..end].trim());
        }
        return None;
    }

    if !trimmed.starts_with('[') {
        return None;
    }

    let end = trimmed.find(']')?;
    let rest = trimmed[end + 1..].trim_start();
    if rest.is_empty() || rest.starts_with('#') {
        Some(trimmed[1..end].trim())
    } else {
        None
    }
}

fn is_dependency_section(header: &str) -> bool {
    matches!(
        header,
        "dependencies" | "dev-dependencies" | "build-dependencies" | "workspace.dependencies"
    ) || header.ends_with(".dependencies")
        || header.ends_with(".dev-dependencies")
        || header.ends_with(".build-dependencies")
}

#[derive(Debug, Eq, PartialEq)]
struct DependencyBody {
    content: String,
    changed: bool,
}

fn rewrite_dependency_body(
    lines: &[&str],
    internal_keys: Option<&BTreeSet<String>>,
) -> DependencyBody {
    let ParsedBody { entries, trailing } = parse_dependency_body(lines);

    let mut internal = Vec::new();
    let mut external = Vec::new();
    for entry in &entries {
        if internal_keys.is_some_and(|keys| keys.contains(entry.key)) {
            internal.push(entry);
        } else {
            external.push(entry);
        }
    }
    internal.sort_by(|a, b| a.key.cmp(b.key));
    external.sort_by(|a, b| a.key.cmp(b.key));

    let canonical_keys = internal
        .iter()
        .chain(external.iter())
        .map(|entry| entry.key);
    if entries.iter().map(|entry| entry.key).eq(canonical_keys) {
        return DependencyBody {
            content: lines.concat(),
            changed: false,
        };
    }

    let mut content = String::new();
    append_group(&mut content, &internal, TrimMode::LeadingAndTrailing);
    if !internal.is_empty() && !external.is_empty() {
        push_blank_separator(&mut content);
    }
    append_group(&mut content, &external, TrimMode::LeadingOnly);
    for line in trailing {
        content.push_str(line);
    }

    DependencyBody {
        content,
        changed: true,
    }
}

struct ParsedBody<'a> {
    entries: Vec<DependencyBlock<'a>>,
    trailing: Vec<&'a str>,
}

fn parse_dependency_body<'a>(lines: &[&'a str]) -> ParsedBody<'a> {
    let mut entries = Vec::new();
    let mut pending = Vec::new();
    let mut current = None;
    let mut nesting = 0;

    for line in lines {
        if let Some(key) = dependency_key(line) {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            let mut entry_lines = std::mem::take(&mut pending);
            entry_lines.push(*line);
            current = Some(DependencyBlock {
                key,
                lines: entry_lines,
            });
            nesting = line_nesting_delta(line).max(0);
            continue;
        }

        if let Some(entry) = current.as_mut() {
            if nesting > 0 {
                entry.lines.push(*line);
                nesting = (nesting + line_nesting_delta(line)).max(0);
            } else if is_prefix_line(line) {
                if let Some(entry) = current.take() {
                    entries.push(entry);
                }
                pending.push(*line);
            } else {
                entry.lines.push(*line);
            }
        } else {
            pending.push(*line);
        }
    }

    if let Some(entry) = current {
        entries.push(entry);
    }

    ParsedBody {
        entries,
        trailing: pending,
    }
}

fn dependency_key(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let eq = trimmed.find('=')?;
    let key = trimmed[..eq].trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return None;
    }

    Some(key)
}

fn line_nesting_delta(line: &str) -> i32 {
    let mut delta = 0;
    let mut quote = None;
    let mut escaped = false;

    for ch in line.chars() {
        if let Some(quote_char) = quote {
            if quote_char == '"' && escaped {
                escaped = false;
                continue;
            }
            if quote_char == '"' && ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == quote_char {
                quote = None;
            }
            continue;
        }

        match ch {
            '"' | '\'' => quote = Some(ch),
            '#' => break,
            '[' | '{' | '(' => delta += 1,
            ']' | '}' | ')' => delta -= 1,
            _ => {}
        }
    }

    delta
}

fn is_prefix_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.is_empty() || trimmed.starts_with('#')
}

fn internal_dependencies(
    metadata: &Metadata,
    manifest: &Path,
    source: &str,
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let members = metadata.workspace_packages();
    let directories: BTreeSet<_> = members
        .iter()
        .filter_map(|package| package.manifest_path.as_std_path().parent())
        .collect();
    let package = members
        .iter()
        .find(|package| package.manifest_path.as_std_path() == manifest);
    let mut sections = BTreeMap::new();
    for header in source
        .lines()
        .filter_map(table_header)
        .filter(|header| is_dependency_section(header))
    {
        if header == "workspace.dependencies" {
            let document: Value = toml::from_str(source).context("parse workspace dependencies")?;
            let mut keys = BTreeSet::new();
            if let Some(dependencies) = document
                .get("workspace")
                .and_then(|workspace| workspace.get("dependencies"))
                .and_then(Value::as_table)
            {
                let directories = directories
                    .iter()
                    .map(fs::canonicalize)
                    .collect::<std::io::Result<BTreeSet<_>>>()?;
                for (key, value) in dependencies {
                    if let Some(path) = value.get("path").and_then(Value::as_str) {
                        let path = metadata
                            .workspace_root
                            .join(path)
                            .as_std_path()
                            .canonicalize()
                            .with_context(|| format!("resolve workspace dependency {key}"))?;
                        if directories.contains(&path) {
                            keys.insert(key.clone());
                        }
                    }
                }
            }
            sections.insert(header.to_owned(), keys);
            continue;
        }
        let section = header.rsplit('.').next().unwrap_or(header);
        let target = if let Some(target) = header
            .strip_prefix("target.")
            .and_then(|target| target.strip_suffix(&format!(".{section}")))
        {
            let parsed: Value = toml::from_str(&format!("[target.{target}]"))?;
            parsed
                .get("target")
                .and_then(Value::as_table)
                .and_then(|table| table.keys().next())
                .map(|target| target.parse::<Platform>())
                .transpose()?
        } else {
            None
        };
        let mut keys = BTreeSet::new();
        if let Some(package) = package {
            for dependency in &package.dependencies {
                let kind_matches = match dependency.kind {
                    DependencyKind::Normal => section == "dependencies",
                    DependencyKind::Development => section == "dev-dependencies",
                    DependencyKind::Build => section == "build-dependencies",
                    _ => false,
                };
                if kind_matches
                    && dependency.target == target
                    && dependency
                        .path
                        .as_ref()
                        .is_some_and(|path| directories.contains(path.as_std_path()))
                {
                    keys.insert(
                        dependency
                            .rename
                            .as_ref()
                            .unwrap_or(&dependency.name)
                            .clone(),
                    );
                }
            }
        }
        sections.insert(header.to_owned(), keys);
    }
    Ok(sections)
}

#[derive(Clone, Copy)]
enum TrimMode {
    LeadingOnly,
    LeadingAndTrailing,
}

fn append_group(output: &mut String, group: &[&DependencyBlock<'_>], trim_mode: TrimMode) {
    let mut lines: Vec<_> = group
        .iter()
        .flat_map(|entry| entry.lines.iter())
        .copied()
        .collect();
    trim_leading_blank_lines(&mut lines);
    if matches!(trim_mode, TrimMode::LeadingAndTrailing) {
        trim_trailing_blank_lines(&mut lines);
    }
    for line in lines {
        output.push_str(line);
    }
}

fn trim_leading_blank_lines(lines: &mut Vec<&str>) {
    let first_content = lines.iter().position(|line| !line.trim().is_empty());
    if let Some(first_content) = first_content {
        lines.drain(..first_content);
    } else {
        lines.clear();
    }
}

fn trim_trailing_blank_lines(lines: &mut Vec<&str>) {
    let trim_from = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |index| index + 1);
    lines.truncate(trim_from);
}

fn push_blank_separator(output: &mut String) {
    if output.ends_with("\n\n") {
        return;
    }
    if output.ends_with('\n') {
        output.push('\n');
    } else {
        output.push_str("\n\n");
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
    };

    use cargo_metadata::MetadataCommand;

    use super::{RewriteResult, add_workspace_dependency, handwritten_manifest_dirs, table_header};

    fn rewrite_manifest(source: &str) -> RewriteResult {
        let keys = BTreeSet::from(
            [
                "kithara",
                "kithara-app",
                "kithara-core",
                "kithara-stream",
                "kithara-test-utils",
            ]
            .map(str::to_owned),
        );
        let internal: BTreeMap<_, _> = source
            .lines()
            .filter_map(table_header)
            .map(|header| (header.to_owned(), keys.clone()))
            .collect();
        super::rewrite_manifest(source, &internal)
    }

    #[test]
    fn excludes_hakari_manifests_from_cargo_sort() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let handwritten = root.path().join("handwritten");
        let generated = root.path().join("generated");
        fs::create_dir_all(&handwritten)?;
        fs::create_dir_all(&generated)?;
        let handwritten_manifest = handwritten.join("Cargo.toml");
        let generated_manifest = generated.join("Cargo.toml");
        fs::write(&handwritten_manifest, "[dependencies]\nserde = \"1\"\n")?;
        fs::write(
            &generated_manifest,
            "[package]\nname = \"generated\"\n### BEGIN HAKARI SECTION\n[dependencies]\nserde = \"1\"\n### END HAKARI SECTION\n",
        )?;

        assert_eq!(
            handwritten_manifest_dirs(&[handwritten_manifest, generated_manifest])?,
            [handwritten]
        );
        Ok(())
    }

    #[test]
    fn preserves_hakari_bytes_and_orders_handwritten_sections() {
        let generated = "### BEGIN HAKARI SECTION\n[dependencies]\nzstd = { version = \"0.13\", features = [\"zstdmt\", \"arrays\"] }\nanyhow = \"1\"\n\n[build-dependencies]\nserde = \"1\"\nanyhow = \"1\"\n### END HAKARI SECTION\n";
        let input = format!(
            "[dev-dependencies]\nserde = \"1\"\nanyhow = \"1\"\n{generated}[target.'cfg(unix)'.dev-dependencies]\nserde = \"1\"\nanyhow = \"1\"\n"
        );

        let rewritten = rewrite_manifest(&input);

        assert_eq!(
            rewritten.content,
            format!(
                "[dev-dependencies]\nanyhow = \"1\"\nserde = \"1\"\n{generated}[target.'cfg(unix)'.dev-dependencies]\nanyhow = \"1\"\nserde = \"1\"\n"
            )
        );
        assert_eq!(
            rewritten.changed_sections,
            ["dev-dependencies", "target.'cfg(unix)'.dev-dependencies"]
        );
        assert!(
            rewrite_manifest(&rewritten.content)
                .changed_sections
                .is_empty()
        );
    }

    #[test]
    fn adds_workspace_dependency_in_canonical_order() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let prefix = "[package]\nname='consumer'\nversion='0.1.0'\nedition='2024'\n\n[workspace]\nmembers=['crates/kithara-stream']\nexclude=['vendor/anyhow','vendor/fieldwork','vendor/futures']\n\n[workspace.dependencies]\nkithara-stream={path='crates/kithara-stream'}\nanyhow={path='vendor/anyhow'}\nfieldwork={path='vendor/fieldwork'}\nfutures={path='vendor/futures'}\n\n";
        for name in ["kithara-stream", "anyhow", "fieldwork", "futures"] {
            let directory = if name == "kithara-stream" {
                root.path().join("crates").join(name)
            } else {
                root.path().join("vendor").join(name)
            };
            fs::create_dir_all(directory.join("src"))?;
            fs::write(
                directory.join("Cargo.toml"),
                format!(
                    "[package]\nname='{name}'\nversion='0.1.0'\nedition='2024'\n{}",
                    if name == "kithara-stream" {
                        ""
                    } else {
                        "[workspace]\n"
                    }
                ),
            )?;
            fs::write(directory.join("src/lib.rs"), "")?;
        }
        fs::create_dir_all(root.path().join("src"))?;
        fs::write(root.path().join("src/lib.rs"), "")?;
        let input = format!(
            "{prefix}[dependencies]\nkithara-stream = {{ workspace = true }}\n\nanyhow = {{ workspace = true }}\nfutures = {{ workspace = true }}\n"
        );
        let manifest = root.path().join("Cargo.toml");
        fs::write(&manifest, &input)?;
        let metadata = MetadataCommand::new()
            .manifest_path(&manifest)
            .no_deps()
            .exec()?;
        assert_eq!(
            metadata
                .workspace_packages()
                .iter()
                .map(|package| package.name.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["consumer", "kithara-stream"])
        );
        let manifest = metadata
            .workspace_root
            .join("Cargo.toml")
            .as_std_path()
            .to_path_buf();
        let rewritten = add_workspace_dependency(&input, "fieldwork", &manifest, &metadata)?
            .expect("dependency added");
        assert_eq!(
            rewritten,
            format!(
                "{prefix}[dependencies]\nkithara-stream = {{ workspace = true }}\n\nanyhow = {{ workspace = true }}\nfieldwork = {{ workspace = true }}\nfutures = {{ workspace = true }}\n"
            )
        );
        assert!(add_workspace_dependency(&rewritten, "fieldwork", &manifest, &metadata)?.is_none());
        Ok(())
    }

    #[test]
    fn moves_internal_dependencies_before_external_dependencies() {
        let input = "[dependencies]\nanyhow = \"1\"\n# local crates\nkithara-core = { workspace = true }\n\n# async runtime\ntokio = \"1\"\n";

        let rewritten = rewrite_manifest(input);

        assert_eq!(
            rewritten.content,
            "[dependencies]\n# local crates\nkithara-core = { workspace = true }\n\nanyhow = \"1\"\n\n# async runtime\ntokio = \"1\"\n"
        );
        assert_eq!(rewritten.changed_sections, vec!["dependencies"]);
    }

    #[test]
    fn leaves_already_ordered_dependencies_untouched() {
        let input = "[dev-dependencies]\nkithara = { path = \".\" }\nanyhow = \"1\"\n# comment\nserde = \"1\"\n";

        let rewritten = rewrite_manifest(input);

        assert_eq!(rewritten.content, input);
        assert!(rewritten.changed_sections.is_empty());
    }

    #[test]
    fn handles_workspace_and_target_dependency_sections() {
        let input = "[workspace.dependencies]\nserde = \"1\"\nkithara = { path = \"crates/kithara\" }\n\n[target.'cfg(unix)'.dev-dependencies]\ntempfile = \"3\"\nkithara-test-utils = { workspace = true }\n";

        let rewritten = rewrite_manifest(input);

        assert_eq!(
            rewritten.content,
            "[workspace.dependencies]\nkithara = { path = \"crates/kithara\" }\n\nserde = \"1\"\n\n[target.'cfg(unix)'.dev-dependencies]\nkithara-test-utils = { workspace = true }\n\ntempfile = \"3\"\n"
        );
        assert_eq!(
            rewritten.changed_sections,
            vec![
                "workspace.dependencies",
                "target.'cfg(unix)'.dev-dependencies"
            ]
        );
    }

    #[test]
    fn preserves_multiline_dependency_entries() {
        let input = "[dependencies]\nweb-sys = { workspace = true, features = [\n    \"AudioContext\",\n] }\nkithara-stream = { workspace = true }\n";

        let rewritten = rewrite_manifest(input);

        assert_eq!(
            rewritten.content,
            "[dependencies]\nkithara-stream = { workspace = true }\n\nweb-sys = { workspace = true, features = [\n    \"AudioContext\",\n] }\n"
        );
    }

    #[test]
    fn sorts_internal_and_external_dependency_groups() {
        let input = "[dependencies]\nkithara-stream = { workspace = true }\nkithara-app = { workspace = true }\nserde = \"1\"\nanyhow = \"1\"\n";

        let rewritten = rewrite_manifest(input);

        assert_eq!(
            rewritten.content,
            "[dependencies]\nkithara-app = { workspace = true }\nkithara-stream = { workspace = true }\n\nanyhow = \"1\"\nserde = \"1\"\n"
        );
        assert_eq!(rewritten.changed_sections, vec!["dependencies"]);
    }

    #[test]
    fn parses_headers_with_trailing_comments() {
        assert_eq!(table_header("[dependencies] # deps"), Some("dependencies"));
        assert_eq!(table_header("[[bin]] # binary"), Some("bin"));
        assert_eq!(table_header("[dependencies] trailing"), None);
    }
}
