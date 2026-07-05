use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;
use regex::Regex;
use rustdoc_types::{Crate, ItemEnum};

use crate::config::DocgenConfig;

pub(crate) fn run(check: bool, docgen: &DocgenConfig) -> Result<()> {
    let root = workspace_root()?;
    validate(docgen)?;

    build_rustdoc_json(&root, docgen)?;
    let json_path = root
        .join("target/doc")
        .join(format!("{}.json", json_stem(docgen)));
    let json = fs::read_to_string(&json_path)
        .with_context(|| format!("read rustdoc JSON {}", json_path.display()))?;
    let krate: Crate = serde_json::from_str(&json)
        .with_context(|| format!("parse rustdoc JSON {}", json_path.display()))?;

    if krate.format_version != rustdoc_types::FORMAT_VERSION {
        bail!(
            "rustdoc JSON format_version {} != rustdoc-types {}; bump the pinned nightly and rustdoc-types together",
            krate.format_version,
            rustdoc_types::FORMAT_VERSION
        );
    }

    let pages = render_pages(&krate, docgen)?;
    // Completeness gate runs on every invocation, so a plain `docgen` (as
    // `just apple doc` calls it) also refuses to build docs for an
    // undocumented public API; `--check` only skips writing the files.
    let documented = check_swift_docs(&root, &docgen.swift_dirs)?;

    if check {
        let temp = TempDir::create(&format!(
            "{}-apple-docgen",
            kithara_xtask_core::util::project_name()
        ))?;
        write_pages(temp.path(), &pages)?;
        println!(
            "docgen --check: {}/{} allowlisted symbols covered, {documented} public Swift symbols documented, format_version={}",
            pages.len(),
            docgen.symbols.len(),
            krate.format_version
        );
        return Ok(());
    }

    let output_dir = root.join(&docgen.output_dir);
    write_pages(&output_dir, &pages)?;
    println!(
        "docgen: wrote {}/{} allowlisted symbols to {} ({documented} public Swift symbols documented)",
        pages.len(),
        docgen.symbols.len(),
        output_dir.display()
    );
    Ok(())
}

fn validate(docgen: &DocgenConfig) -> Result<()> {
    if docgen.package.is_empty() {
        bail!("[ext.apple.docgen] package is empty in .config/xtask.toml");
    }
    if docgen.module.is_empty() {
        bail!("[ext.apple.docgen] module is empty in .config/xtask.toml");
    }
    if docgen.output_dir.is_empty() {
        bail!("[ext.apple.docgen] output_dir is empty in .config/xtask.toml");
    }
    if docgen.symbols.is_empty() {
        bail!("[ext.apple.docgen] has no symbols in .config/xtask.toml");
    }
    Ok(())
}

fn json_stem(docgen: &DocgenConfig) -> String {
    docgen.package.replace('-', "_")
}

fn workspace_root() -> Result<PathBuf> {
    let metadata = MetadataCommand::new()
        .exec()
        .context("failed to read cargo metadata")?;
    Ok(metadata.workspace_root.into_std_path_buf())
}

fn build_rustdoc_json(root: &Path, docgen: &DocgenConfig) -> Result<()> {
    let mut args = vec![
        "rustdoc".to_string(),
        "-p".to_string(),
        docgen.package.clone(),
    ];
    if !docgen.features.is_empty() {
        args.push("--features".to_string());
        args.push(docgen.features.join(","));
    }
    args.extend([
        "--".to_string(),
        "-Zunstable-options".to_string(),
        "--output-format".to_string(),
        "json".to_string(),
    ]);

    let status = Command::new("cargo")
        .args(&args)
        .env("RUSTC_BOOTSTRAP", "1")
        .current_dir(root)
        .status()
        .with_context(|| format!("failed to run cargo rustdoc for {}", docgen.package))?;
    if !status.success() {
        bail!(
            "cargo rustdoc for {} failed with status {status}",
            docgen.package
        );
    }
    Ok(())
}

struct Page {
    file_name: String,
    contents: String,
}

fn render_pages(krate: &Crate, docgen: &DocgenConfig) -> Result<Vec<Page>> {
    let link = Regex::new(r"\[`([^`]+)`\]").context("compile rustdoc link regex")?;
    let mut pages = Vec::with_capacity(docgen.symbols.len());
    for symbol in &docgen.symbols {
        let docs = docs_for_item(krate, &symbol.rust)?;
        let (abstract_text, discussion) = split_docs(&flatten_rustdoc_links(&link, &docs));
        if abstract_text.trim().is_empty() {
            bail!(
                "rustdoc item {} for DocC {} has an empty abstract",
                symbol.rust,
                symbol.docc
            );
        }

        let mut contents = String::new();
        contents.push_str("# ``");
        contents.push_str(&docgen.module);
        contents.push('/');
        contents.push_str(&symbol.docc);
        contents.push_str("``\n\n");
        contents.push_str("@Metadata {\n");
        contents.push_str("    @DocumentationExtension(mergeBehavior: override)\n");
        contents.push_str("}\n\n");
        contents.push_str(&abstract_text);
        contents.push('\n');
        if !discussion.trim().is_empty() {
            contents.push('\n');
            contents.push_str(&discussion);
            contents.push('\n');
        }

        pages.push(Page {
            file_name: format!("{}.md", symbol.docc),
            contents,
        });
    }
    Ok(pages)
}

fn docs_for_item(krate: &Crate, rust_name: &str) -> Result<String> {
    let mut matches = krate.index.values().filter(|item| {
        item.name.as_deref() == Some(rust_name)
            && matches!(item.inner, ItemEnum::Enum(_) | ItemEnum::Struct(_))
    });

    let item = matches
        .next()
        .with_context(|| format!("rustdoc item {rust_name} not found"))?;
    if matches.next().is_some() {
        bail!("rustdoc item name {rust_name} is ambiguous");
    }

    item.docs
        .as_ref()
        .map(|docs| docs.trim().to_string())
        .filter(|docs| !docs.is_empty())
        .with_context(|| format!("rustdoc item {rust_name} has empty docs"))
}

fn flatten_rustdoc_links(link: &Regex, docs: &str) -> String {
    link.replace_all(docs, |captures: &regex::Captures<'_>| {
        format!("`{}`", final_path_segment(&captures[1]))
    })
    .into_owned()
}

fn final_path_segment(path: &str) -> &str {
    let trimmed = path.trim().trim_end_matches("()");
    trimmed
        .rsplit("::")
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(trimmed)
}

fn split_docs(docs: &str) -> (String, String) {
    let normalized = docs.replace("\r\n", "\n");
    if let Some((first, rest)) = normalized.split_once("\n\n") {
        (first.trim().to_string(), rest.trim().to_string())
    } else {
        (normalized.trim().to_string(), String::new())
    }
}

fn write_pages(output_dir: &Path, pages: &[Page]) -> Result<()> {
    fs::create_dir_all(output_dir).with_context(|| format!("create {}", output_dir.display()))?;
    for page in pages {
        let path = output_dir.join(&page.file_name);
        fs::write(&path, &page.contents).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

/// Completeness gate: every `public`/`open` declaration in the configured
/// Swift dirs must carry a `///` (or `/** */`) doc comment, so no public
/// symbol ships undocumented. Returns the count of public symbols checked.
fn check_swift_docs(root: &Path, dirs: &[String]) -> Result<usize> {
    let decl = Regex::new(
        r"\b(?:public|open)\b[^/\n]*\b(?:func|var|let|struct|enum|class|protocol|typealias|init|subscript|actor|associatedtype)\b",
    )
    .context("compile swift public-decl regex")?;

    let mut files = Vec::new();
    for dir in dirs {
        collect_swift(&root.join(dir), &mut files)
            .with_context(|| format!("walk swift dir {dir}"))?;
    }
    files.sort();

    let mut checked = 0usize;
    let mut undocumented = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with("//") || !decl.is_match(line) {
                continue;
            }
            checked += 1;
            if !documented_above(&lines, i) {
                undocumented.push(format!(
                    "{}:{}: {}",
                    file.display(),
                    i + 1,
                    symbol_label(line)
                ));
            }
        }
    }

    if !undocumented.is_empty() {
        bail!(
            "docgen --check: {} public Swift symbol(s) missing a /// doc comment:\n  {}",
            undocumented.len(),
            undocumented.join("\n  ")
        );
    }
    Ok(checked)
}

fn collect_swift(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).with_context(|| format!("read dir {}", dir.display()))? {
        let path = entry?.path();
        let ext = path.extension().and_then(|e| e.to_str());
        if path.is_dir() {
            if ext != Some("docc") {
                collect_swift(&path, out)?;
            }
        } else if ext == Some("swift") {
            out.push(path);
        }
    }
    Ok(())
}

/// A `///` (or block `/** */`) doc directly above the declaration, allowing
/// attribute lines in between. A blank line or code breaks the association.
fn documented_above(lines: &[&str], decl_index: usize) -> bool {
    let mut j = decl_index;
    while j > 0 {
        j -= 1;
        let t = lines[j].trim();
        if t.starts_with("///") || t.ends_with("*/") {
            return true;
        }
        if t.starts_with('@') {
            continue;
        }
        return false;
    }
    false
}

fn symbol_label(decl_line: &str) -> String {
    decl_line
        .split("//")
        .next()
        .unwrap_or(decl_line)
        .trim()
        .trim_end_matches('{')
        .trim()
        .to_string()
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn create(prefix: &str) -> Result<Self> {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before UNIX_EPOCH")?;
        let path = env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            epoch.as_nanos()
        ));
        fs::create_dir_all(&path).with_context(|| format!("create {}", path.display()))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
