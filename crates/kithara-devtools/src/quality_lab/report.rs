use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::Serialize;

use super::manifest::{Profile, ToolManifest};

#[derive(Serialize)]
struct Summary<'a> {
    schema_version: u32,
    revision: &'a str,
    profile: Profile,
    tools: &'a [ToolManifest],
}

pub(super) struct SummaryPaths {
    pub(super) json: std::path::PathBuf,
    pub(super) markdown: std::path::PathBuf,
}

pub(super) fn write(
    directory: &Path,
    revision: &str,
    profile: Profile,
    manifests: &[ToolManifest],
) -> Result<SummaryPaths> {
    fs::create_dir_all(directory)
        .with_context(|| format!("create Quality Lab output: {}", directory.display()))?;
    let stem = format!("{profile}-summary");
    let json_path = directory.join(format!("{stem}.json"));
    let markdown_path = directory.join(format!("{stem}.md"));
    let summary = Summary {
        schema_version: 1,
        revision,
        profile,
        tools: manifests,
    };
    let json = serde_json::to_string_pretty(&summary).context("serialize Quality Lab summary")?;
    fs::write(&json_path, format!("{json}\n"))
        .with_context(|| format!("write Quality Lab summary: {}", json_path.display()))?;
    fs::write(
        &markdown_path,
        render_markdown(revision, profile, manifests),
    )
    .with_context(|| {
        format!(
            "write Quality Lab Markdown summary: {}",
            markdown_path.display()
        )
    })?;
    Ok(SummaryPaths {
        json: json_path,
        markdown: markdown_path,
    })
}

fn render_markdown(revision: &str, profile: Profile, manifests: &[ToolManifest]) -> String {
    let mut markdown = format!(
        "# Quality Lab\n\n- Revision: `{revision}`\n- Profile: `{profile}`\n\n\
         | Tool | Version | Status | Duration (ms) | Note |\n\
         | --- | --- | --- | ---: | --- |\n"
    );
    for manifest in manifests {
        let note = manifest
            .note
            .as_deref()
            .unwrap_or("")
            .replace('|', "\\|")
            .replace('\n', " ");
        markdown.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            manifest.tool, manifest.tool_version, manifest.status, manifest.duration_ms, note
        ));
    }
    markdown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quality_lab::manifest::{Status, Tool};

    #[test]
    fn markdown_preserves_stable_tool_order_and_statuses() {
        let manifests = vec![
            manifest(Tool::Cha, Status::Findings),
            manifest(Tool::Rustqual, Status::Clean),
        ];

        let markdown = render_markdown("abc123", Profile::Scheduled, &manifests);

        let cha = markdown.find("| cha |").expect("Cha row");
        let rustqual = markdown.find("| rustqual |").expect("rustqual row");
        assert!(cha < rustqual);
        assert!(markdown.contains("| cha | 1.0.0 | findings |"));
        assert!(markdown.contains("| rustqual | 1.0.0 | clean |"));
    }

    fn manifest(tool: Tool, status: Status) -> ToolManifest {
        ToolManifest {
            schema_version: 1,
            revision: "abc123".to_owned(),
            profile: Profile::Scheduled,
            tool,
            tool_version: "1.0.0".to_owned(),
            status,
            duration_ms: 10,
            invocations: Vec::new(),
            note: None,
        }
    }
}
