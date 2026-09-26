use std::{fs, path::Path};

use anyhow::{Context as _, Result};
use toml::Value;

use super::{Check, Context};
use crate::{
    common::{
        violation::Violation,
        walker::{compile_globs, matches_any_segmented, relative_to},
    },
    style::config::ReadmeShapeConfig,
};

pub(crate) mod consts {
    pub(crate) const ID: &str = "readme_shape";
}

pub(crate) struct ReadmeShape;

/// The manifest facts a crate README must agree with.
struct Package {
    license: String,
    name: String,
    published: bool,
}

impl Check for ReadmeShape {
    fn id(&self) -> &'static str {
        consts::ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.readme_shape;
        let workspace_license = workspace_license(ctx.workspace_root)?;
        let mut violations = Vec::new();
        for path in ctx.scan.text_files(ctx.scope)?.iter() {
            let rel = relative_to(ctx.workspace_root, path)
                .to_string_lossy()
                .replace('\\', "/");
            if !selected(cfg, &rel) {
                continue;
            }
            let Some(src) = ctx.scan.source(path) else {
                continue;
            };
            let manifest = path.with_file_name("Cargo.toml");
            let package = package(&manifest, &workspace_license)?;
            violations.extend(scan_content(cfg, &rel, &src, &package));
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }

    fn uses_global_lint_excludes(&self) -> bool {
        false
    }
}

/// Whether the shape contract covers this document.
fn selected(cfg: &ReadmeShapeConfig, rel: &str) -> bool {
    let path = Path::new(rel);
    matches_any_segmented(&compile_globs(&cfg.include_globs), path)
        && !matches_any_segmented(&compile_globs(&cfg.exclude_paths), path)
}

/// The license every crate inherits unless its own manifest names another.
fn workspace_license(workspace_root: &Path) -> Result<String> {
    let manifest = workspace_root.join("Cargo.toml");
    let doc = manifest_document(&manifest)?;
    let license = doc
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("license"))
        .and_then(Value::as_str)
        .with_context(|| {
            format!(
                "workspace manifest names no license: {}",
                manifest.display()
            )
        })?;
    Ok(license.to_string())
}

fn manifest_document(manifest: &Path) -> Result<Value> {
    let text = fs::read_to_string(manifest)
        .with_context(|| format!("read crate manifest: {}", manifest.display()))?;
    toml::from_str(&text).with_context(|| format!("parse crate manifest: {}", manifest.display()))
}

fn package(manifest: &Path, workspace_license: &str) -> Result<Package> {
    let doc = manifest_document(manifest)?;
    let package = doc.get("package").with_context(|| {
        format!(
            "crate manifest has no package table: {}",
            manifest.display()
        )
    })?;
    let name = package
        .get("name")
        .and_then(Value::as_str)
        .with_context(|| format!("crate manifest has no package name: {}", manifest.display()))?;
    Ok(Package {
        license: package
            .get("license")
            .and_then(Value::as_str)
            .unwrap_or(workspace_license)
            .to_string(),
        name: name.to_string(),
        published: package
            .get("publish")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

fn scan_content(
    cfg: &ReadmeShapeConfig,
    rel: &str,
    src: &str,
    package: &Package,
) -> Vec<Violation> {
    let document = Document::parse(src);
    let mut violations = Vec::new();
    violations.extend(header_violations(rel, &document, package));
    violations.extend(title_violations(rel, &document, package));
    violations.extend(lead_violations(rel, &document));
    violations.extend(section_violations(cfg, rel, &document));
    violations
}

fn header_violations(rel: &str, document: &Document<'_>, package: &Package) -> Vec<Violation> {
    let mut violations = Vec::new();
    if !document.preamble.contains("logo.svg") {
        violations.push(Violation::deny(
            consts::ID,
            format!("{rel}:header"),
            format!("{rel} does not open with the shared header: the centered logo, then the badge block"),
        ));
    }
    for (marker, badge) in registry_badges(&package.name) {
        let present = document.preamble.contains(&marker);
        if package.published && !present {
            violations.push(Violation::deny(
                consts::ID,
                format!("{rel}:badge:{badge}"),
                format!("{rel} is published and its header carries no {badge} badge"),
            ));
        } else if !package.published && present {
            violations.push(Violation::deny(
                consts::ID,
                format!("{rel}:badge:{badge}"),
                format!("{rel} is `publish = false` and its header advertises a {badge} page"),
            ));
        }
    }
    let license = &package.license;
    if !document.preamble.contains(&license_badge(license)) {
        violations.push(Violation::deny(
            consts::ID,
            format!("{rel}:badge:license"),
            format!("{rel} header carries no `{license}` license badge, the license its manifest declares"),
        ));
    }
    if document.preamble.contains("../") {
        violations.push(Violation::deny(
            consts::ID,
            format!("{rel}:header:escape"),
            format!("{rel} header reaches outside the package with `../`; crates.io and docs.rs render this README where that path does not exist, so the logo and the license link are repository URLs"),
        ));
    }
    violations
}

/// The shields.io spelling of an SPDX expression: a dual license reads as a
/// slash, and every literal hyphen is doubled.
fn license_badge(license: &str) -> String {
    let escaped = license.replace('-', "--").replace(" OR ", "%2F");
    format!("badge/license-{escaped}-")
}

fn registry_badges(name: &str) -> [(String, &'static str); 2] {
    [
        (format!("crates.io/crates/{name}"), "crates.io"),
        (format!("docs.rs/{name}/badge.svg"), "docs.rs"),
    ]
}

fn title_violations(rel: &str, document: &Document<'_>, package: &Package) -> Vec<Violation> {
    let mut violations = Vec::new();
    match document.title {
        None => violations.push(Violation::deny(
            consts::ID,
            format!("{rel}:title"),
            format!("{rel} has no `# ` title"),
        )),
        Some(title) if title != package.name => violations.push(Violation::deny(
            consts::ID,
            format!("{rel}:title"),
            format!(
                "{rel} is titled `{title}`, not the package name `{}`",
                package.name
            ),
        )),
        Some(_) => {}
    }
    violations
}

fn lead_violations(rel: &str, document: &Document<'_>) -> Vec<Violation> {
    if document.has_lead {
        return Vec::new();
    }
    vec![Violation::deny(
        consts::ID,
        format!("{rel}:lead"),
        format!("{rel} states no role between its title and its first section"),
    )]
}

fn section_violations(
    cfg: &ReadmeShapeConfig,
    rel: &str,
    document: &Document<'_>,
) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut previous = None;
    for section in &document.sections {
        let Some(rank) = cfg.sections.iter().position(|known| known == section) else {
            violations.push(Violation::deny(
                consts::ID,
                format!("{rel}:section:{section}"),
                format!(
                    "{rel} carries the top-level section `{section}`; the template allows {}, and anything else nests under one of them as `###`",
                    cfg.sections.join(", ")
                ),
            ));
            continue;
        };
        if previous.is_some_and(|previous| rank <= previous) {
            violations.push(Violation::deny(
                consts::ID,
                format!("{rel}:order:{section}"),
                format!(
                    "{rel} places `{section}` out of template order ({})",
                    cfg.sections.join(" -> ")
                ),
            ));
        }
        previous = Some(rank);
    }
    violations
}

/// The three parts of a crate README the template constrains: what stands
/// above the title, the title itself, and the top-level sections under it.
struct Document<'a> {
    title: Option<&'a str>,
    preamble: String,
    sections: Vec<&'a str>,
    has_lead: bool,
}

impl<'a> Document<'a> {
    fn parse(src: &'a str) -> Self {
        let mut document = Self {
            title: None,
            preamble: String::new(),
            sections: Vec::new(),
            has_lead: false,
        };
        let mut fenced = false;
        for line in src.lines() {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
                continue;
            }
            if fenced {
                continue;
            }
            if let Some(title) = line.strip_prefix("# ") {
                if document.title.is_none() {
                    document.title = Some(title.trim());
                }
                continue;
            }
            if let Some(section) = line.strip_prefix("## ") {
                document.sections.push(section.trim());
                continue;
            }
            if document.title.is_none() {
                document.preamble.push_str(line);
                document.preamble.push('\n');
            } else if document.sections.is_empty() && !line.trim().is_empty() {
                document.has_lead = true;
            }
        }
        document
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ReadmeShapeConfig {
        ReadmeShapeConfig::default()
    }

    fn published() -> Package {
        Package {
            license: "MIT OR Apache-2.0".to_string(),
            name: "kithara-demo".to_string(),
            published: true,
        }
    }

    fn private() -> Package {
        Package {
            published: false,
            ..published()
        }
    }

    fn forked() -> Package {
        Package {
            license: "MPL-2.0".to_string(),
            ..published()
        }
    }

    fn header(package: &Package) -> String {
        let mut header = String::from(
            "<img src=\"https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg\" alt=\"kithara\" width=\"300\">\n\n",
        );
        if package.published {
            header.push_str("[![crates.io](https://img.shields.io/crates/v/kithara-demo.svg)](https://crates.io/crates/kithara-demo)\n");
            header.push_str("[![docs.rs](https://docs.rs/kithara-demo/badge.svg)](https://docs.rs/kithara-demo)\n");
        }
        header.push_str("[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)\n\n");
        header
    }

    fn readme(package: &Package, body: &str) -> String {
        format!(
            "{}# kithara-demo\n\nWhat the crate is.\n\n{body}",
            header(package)
        )
    }

    fn scan(package: &Package, src: &str) -> Vec<Violation> {
        scan_content(&config(), "crates/kithara-demo/README.md", src, package)
    }

    fn keys(package: &Package, src: &str) -> Vec<String> {
        scan(package, src)
            .into_iter()
            .map(|violation| violation.key)
            .collect()
    }

    #[test]
    fn a_readme_on_the_template_is_silent() {
        let src = readme(
            &published(),
            "## Usage\n\nHow.\n\n## Key Types\n\n- `Demo`\n\n## Integration\n\nWho.\n",
        );

        assert!(scan(&published(), &src).is_empty());
    }

    #[test]
    fn a_published_crate_without_registry_badges_is_denied() {
        let src = readme(&private(), "## Usage\n\nHow.\n");

        let keys = keys(&published(), &src);

        assert!(keys.iter().any(|key| key.ends_with(":badge:crates.io")));
        assert!(keys.iter().any(|key| key.ends_with(":badge:docs.rs")));
    }

    #[test]
    fn a_private_crate_advertising_a_registry_page_is_denied() {
        let src = readme(&published(), "## Usage\n\nHow.\n");

        let violations = scan(&private(), &src);

        assert_eq!(violations.len(), 2);
        assert!(violations[0].message.contains("publish = false"));
    }

    #[test]
    fn a_license_badge_that_is_not_the_manifest_license_is_denied() {
        let src = readme(&published(), "## Usage\n\nHow.\n");

        let violations = scan(&forked(), &src);

        assert_eq!(violations.len(), 1);
        assert_eq!(
            violations[0].key,
            "crates/kithara-demo/README.md:badge:license"
        );
        assert!(violations[0].message.contains("MPL-2.0"));
    }

    #[test]
    fn a_header_that_reaches_outside_the_package_is_denied() {
        let src = readme(&published(), "## Usage\n\nHow.\n").replace(
            "https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg",
            "../../logo.svg",
        );

        let violations = scan(&published(), &src);

        assert_eq!(violations.len(), 1);
        assert_eq!(
            violations[0].key,
            "crates/kithara-demo/README.md:header:escape"
        );
    }

    #[test]
    fn a_relative_link_below_the_header_is_left_alone() {
        let src = readme(
            &published(),
            "## Usage\n\nSee [tooling](../../docs/guides/tooling.md).\n",
        );

        assert!(scan(&published(), &src).is_empty());
    }

    #[test]
    fn a_dual_license_reads_as_a_slash_in_the_badge() {
        assert_eq!(
            license_badge("MIT OR Apache-2.0"),
            "badge/license-MIT%2FApache--2.0-"
        );
        assert_eq!(license_badge("MPL-2.0"), "badge/license-MPL--2.0-");
    }

    #[test]
    fn a_readme_without_the_shared_header_is_denied() {
        let src = "# kithara-demo\n\nWhat the crate is.\n\n## Usage\n\nHow.\n";

        let keys = keys(&published(), src);

        assert!(keys.iter().any(|key| key.ends_with(":header")));
    }

    #[test]
    fn a_title_that_is_not_the_package_name_is_denied() {
        let src = readme(&published(), "## Usage\n\nHow.\n").replace("# kithara-demo", "# Demo");

        let violations = scan(&published(), &src);

        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("not the package name"));
    }

    #[test]
    fn a_readme_that_states_no_role_is_denied() {
        let src = format!(
            "{}# kithara-demo\n\n## Usage\n\nHow.\n",
            header(&published())
        );

        let violations = scan(&published(), &src);

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].key, "crates/kithara-demo/README.md:lead");
    }

    #[test]
    fn a_section_outside_the_template_is_denied() {
        let src = readme(&published(), "## Usage\n\nHow.\n\n## Backends\n\nWhich.\n");

        let violations = scan(&published(), &src);

        assert_eq!(violations.len(), 1);
        assert_eq!(
            violations[0].key,
            "crates/kithara-demo/README.md:section:Backends"
        );
    }

    #[test]
    fn template_sections_out_of_order_are_denied() {
        let src = readme(&published(), "## Integration\n\nWho.\n\n## Usage\n\nHow.\n");

        let violations = scan(&published(), &src);

        assert_eq!(violations.len(), 1);
        assert_eq!(
            violations[0].key,
            "crates/kithara-demo/README.md:order:Usage"
        );
    }

    #[test]
    fn a_repeated_template_section_is_denied() {
        let src = readme(&published(), "## Usage\n\nHow.\n\n## Usage\n\nAgain.\n");

        let violations = scan(&published(), &src);

        assert_eq!(violations.len(), 1);
        assert_eq!(
            violations[0].key,
            "crates/kithara-demo/README.md:order:Usage"
        );
    }

    #[test]
    fn a_heading_inside_a_fence_is_not_a_section() {
        let src = readme(
            &published(),
            "## Usage\n\n```toml\n## Backends\n```\n\n## Integration\n\nWho.\n",
        );

        assert!(scan(&published(), &src).is_empty());
    }

    #[test]
    fn a_subsection_carries_content_outside_the_template() {
        let src = readme(
            &published(),
            "## Usage\n\nHow.\n\n### Backends\n\nWhich.\n\n## Integration\n\nWho.\n",
        );

        assert!(scan(&published(), &src).is_empty());
    }

    #[test]
    fn the_contract_covers_crate_readmes_only() {
        assert!(selected(&config(), "crates/kithara-demo/README.md"));
        assert!(!selected(&config(), "crates/kithara-demo/ARCHITECTURE.md"));
        assert!(!selected(&config(), "docs/README.md"));
        assert!(!selected(
            &config(),
            "crates/kithara-demo/src/sub/README.md"
        ));
    }
}
