use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;
use clap::{Args, Subcommand};

use crate::{Ctx, util::ensure_clean_tree};

#[derive(Debug, Args)]
pub struct ManifestArgs {
    #[command(subcommand)]
    pub command: ManifestCommand,
}

#[derive(Debug, Subcommand)]
pub enum ManifestCommand {
    /// Keep internal kithara dependencies before external dependencies.
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

pub(crate) fn run(args: &ManifestArgs, _ctx: &Ctx) -> Result<()> {
    match &args.command {
        ManifestCommand::DependencyOrder(args) => dependency_order(args),
    }
}

fn dependency_order(args: &DependencyOrderArgs) -> Result<()> {
    if args.fix {
        ensure_clean_tree(args.allow_dirty, "manifest dependency-order")?;
    }

    let manifest_set = workspace_manifests()?;
    let mut changed = Vec::new();

    for manifest in &manifest_set.manifests {
        let original =
            fs::read_to_string(manifest).with_context(|| format!("read {}", manifest.display()))?;
        let rewritten = rewrite_manifest(&original);
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
        "Cargo manifests must list internal kithara dependencies before external dependencies and keep each dependency group sorted:\n{}",
        changed.join("\n")
    )
}

fn workspace_manifests() -> Result<ManifestSet> {
    let metadata = MetadataCommand::new()
        .no_deps()
        .exec()
        .context("load cargo metadata")?;
    let workspace_root = metadata.workspace_root.as_std_path().to_path_buf();
    let mut manifests = BTreeSet::new();

    manifests.insert(workspace_root.join("Cargo.toml"));
    for package in metadata.workspace_packages() {
        manifests.insert(package.manifest_path.as_std_path().to_path_buf());
    }

    Ok(ManifestSet {
        workspace_root,
        manifests: manifests.into_iter().collect(),
    })
}

fn display_path(workspace_root: &Path, path: &Path) -> String {
    path.strip_prefix(workspace_root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn rewrite_manifest(source: &str) -> RewriteResult {
    let lines: Vec<_> = source.split_inclusive('\n').collect();
    let mut output = String::with_capacity(source.len());
    let mut changed_sections = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let Some(header) = table_header(lines[index]) else {
            output.push_str(lines[index]);
            index += 1;
            continue;
        };

        output.push_str(lines[index]);
        let body_start = index + 1;
        let body_end = section_body_end(&lines, body_start);

        if is_dependency_section(header) {
            let body = rewrite_dependency_body(&lines[body_start..body_end]);
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

fn section_body_end(lines: &[&str], body_start: usize) -> usize {
    lines[body_start..]
        .iter()
        .position(|line| table_header(line).is_some())
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

fn rewrite_dependency_body(lines: &[&str]) -> DependencyBody {
    let ParsedBody { entries, trailing } = parse_dependency_body(lines);

    let mut internal = Vec::new();
    let mut external = Vec::new();
    for entry in &entries {
        if is_internal_dependency(entry.key) {
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

fn is_internal_dependency(key: &str) -> bool {
    key == "kithara" || key.starts_with("kithara-")
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
    use super::{rewrite_manifest, table_header};

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
