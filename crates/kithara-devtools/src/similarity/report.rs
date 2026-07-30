use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Serialize;

use super::{
    Direction, Profile, Substitution,
    analysis::{AbstractionRef, AnalysisReport, Candidate, Recommendation},
};

const SCHEMA_VERSION: u32 = 2;

pub(super) struct ArtifactSet {
    pub(super) document: PathBuf,
}

#[derive(Serialize)]
struct Graph<'a> {
    schema_version: u32,
    nodes: Vec<GraphNode<'a>>,
    edges: Vec<GraphEdge>,
}

#[derive(Serialize)]
struct GraphNode<'a> {
    id: String,
    abstraction: &'a AbstractionRef,
}

#[derive(Serialize)]
struct GraphEdge {
    source: String,
    target: String,
    state_similarity: f64,
    behavior_similarity: Option<f64>,
    overall_similarity: f64,
    recommendation: Recommendation,
}

#[derive(Serialize)]
struct Manifest<'a> {
    schema_version: u32,
    revision: &'a str,
    profile: Profile,
    roots: &'a [String],
    include_default_excluded: bool,
    status: &'static str,
    scanned_files: usize,
    abstractions: usize,
    candidates: usize,
    interned_shapes: usize,
    cached_comparisons: usize,
    files: BTreeMap<&'static str, &'static str>,
}

pub(super) fn write(
    output: &Path,
    revision: &str,
    profile: Profile,
    roots: &[String],
    include_default_excluded: bool,
    report: &AnalysisReport,
) -> Result<ArtifactSet> {
    fs::create_dir_all(output)
        .with_context(|| format!("create similarity output: {}", output.display()))?;
    write_json(&output.join("report.json"), report)?;
    write_json(&output.join("graph.json"), &graph(report))?;
    write_json(
        &output.join("manifest.json"),
        &Manifest {
            schema_version: SCHEMA_VERSION,
            revision,
            profile,
            roots,
            include_default_excluded,
            status: if report.candidates.is_empty() {
                "clean"
            } else {
                "findings"
            },
            scanned_files: report.scanned_files,
            abstractions: report.abstractions,
            candidates: report.candidates.len(),
            interned_shapes: report.interned_shapes,
            cached_comparisons: report.cached_comparisons,
            files: BTreeMap::from([
                ("document", "report.md"),
                ("graph", "graph.json"),
                ("manifest", "manifest.json"),
                ("report", "report.json"),
            ]),
        },
    )?;
    let document = output.join("report.md");
    write_text(&document, &markdown(revision, profile, report))?;
    Ok(ArtifactSet { document })
}

fn graph(report: &AnalysisReport) -> Graph<'_> {
    let mut nodes = BTreeMap::new();
    for candidate in &report.candidates {
        for abstraction in [&candidate.left, &candidate.right] {
            nodes
                .entry(node_id(abstraction))
                .or_insert_with(|| abstraction);
        }
    }
    Graph {
        schema_version: SCHEMA_VERSION,
        nodes: nodes
            .into_iter()
            .map(|(id, abstraction)| GraphNode { id, abstraction })
            .collect(),
        edges: report
            .candidates
            .iter()
            .map(|candidate| GraphEdge {
                source: node_id(&candidate.left),
                target: node_id(&candidate.right),
                state_similarity: candidate.state_similarity,
                behavior_similarity: candidate.behavior_similarity,
                overall_similarity: candidate.overall_similarity,
                recommendation: candidate.recommendation,
            })
            .collect(),
    }
}

fn markdown(revision: &str, profile: Profile, report: &AnalysisReport) -> String {
    let mut output = format!(
        "# Behavioral similarity\n\n\
         - Revision: `{revision}`\n\
         - Profile: `{}`\n\
         - Rust files: {}\n\
         - Abstractions: {}\n\
         - Candidates: {}\n\
         - Interned type shapes: {}\n\
         - Cached type comparisons: {}\n\n\
         ## Candidate map\n\n\
         ```mermaid\n\
         flowchart LR\n",
        profile.as_str(),
        report.scanned_files,
        report.abstractions,
        report.candidates.len(),
        report.interned_shapes,
        report.cached_comparisons,
    );
    let aggregates = crate_aggregates(report);
    let crates = aggregates
        .keys()
        .flat_map(|(left, right)| [left.clone(), right.clone()])
        .collect::<std::collections::BTreeSet<_>>();
    let crate_ids = crates
        .into_iter()
        .enumerate()
        .map(|(index, name)| (name, format!("c{index}")))
        .collect::<BTreeMap<_, _>>();
    for (name, id) in &crate_ids {
        output.push_str(&format!("    {id}[\"{}\"]\n", mermaid_text(name)));
    }
    for ((left, right), aggregate) in aggregates {
        output.push_str(&format!(
            "    {} -->|\"{} candidates, max {:.0}%\"| {}\n",
            crate_ids[&left],
            aggregate.count,
            aggregate.maximum * 100.0,
            crate_ids[&right],
        ));
    }
    output.push_str("```\n\n## Candidates\n\n");
    if report.candidates.is_empty() {
        output.push_str("No structural or behavioral similarity candidates were found.\n");
        return output;
    }
    for (index, candidate) in report.candidates.iter().enumerate() {
        output.push_str(&candidate_markdown(index + 1, candidate));
    }
    output.push_str(
        "## Interpretation\n\n\
         Scores are candidate evidence, not proof of substitutability. Type caveats and behavior \
         differences must be checked before refactoring. Source derives are ignored as decoration; \
         generated proc-macro output is not expanded.\n",
    );
    output
}

struct CrateAggregate {
    count: usize,
    maximum: f64,
}

fn crate_aggregates(report: &AnalysisReport) -> BTreeMap<(String, String), CrateAggregate> {
    let mut aggregates: BTreeMap<(String, String), CrateAggregate> = BTreeMap::new();
    for candidate in &report.candidates {
        let left = crate_name(&candidate.left.path);
        let right = crate_name(&candidate.right.path);
        let key = if left <= right {
            (left, right)
        } else {
            (right, left)
        };
        let aggregate = aggregates.entry(key).or_insert(CrateAggregate {
            count: 0,
            maximum: 0.0,
        });
        aggregate.count += 1;
        aggregate.maximum = aggregate.maximum.max(candidate.overall_similarity);
    }
    aggregates
}

fn crate_name(path: &str) -> String {
    let mut components = path.split('/');
    if components.next() == Some("crates") {
        return components.next().unwrap_or("workspace").to_string();
    }
    path.split('/').next().unwrap_or("workspace").to_string()
}

fn candidate_markdown(index: usize, candidate: &Candidate) -> String {
    let behavior = candidate.behavior_similarity.map_or_else(
        || "n/a".to_string(),
        |score| format!("{:.1}%", score * 100.0),
    );
    let mut output = format!(
        "### {index}. `{}` and `{}`\n\n\
         - Locations: `{}:{}` and `{}:{}`\n\
         - Recommendation: **{}**\n\
         - State: {:.1}%; behavior: {behavior}; combined: {:.1}%\n",
        candidate.left.name,
        candidate.right.name,
        candidate.left.path,
        candidate.left.line,
        candidate.right.path,
        candidate.right.line,
        recommendation_name(candidate.recommendation),
        candidate.state_similarity * 100.0,
        candidate.overall_similarity * 100.0,
    );
    if !candidate.field_matches.is_empty() {
        output.push_str("- Matched fields: ");
        output.push_str(
            &candidate
                .field_matches
                .iter()
                .map(|field| {
                    format!(
                        "`{} <-> {}` (type {:.0}%, name {:.0}%, field {:.0}%)",
                        field.left,
                        field.right,
                        field.type_similarity * 100.0,
                        field.name_similarity * 100.0,
                        field.match_similarity * 100.0,
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
        );
        output.push_str(".\n");
    }
    output.push_str("\n#### Type substitutions\n\n");
    if candidate.substitutions.is_empty() {
        output.push_str("No container or ownership substitution is required.\n\n");
    } else {
        for substitution in &candidate.substitutions {
            let caveats = if substitution.caveats.is_empty() {
                "none".to_string()
            } else {
                substitution.caveats.join(", ")
            };
            output.push_str(&format!(
                "- `{}` <-> `{}`: {}, {}, source `{}`; caveats: {}.\n",
                substitution.left,
                substitution.right,
                substitution_name(substitution.substitution),
                direction_name(substitution.direction),
                substitution.source,
                caveats,
            ));
        }
        output.push('\n');
    }
    if !candidate.shared_behavior.is_empty() {
        output.push_str("#### Shared behavior evidence\n\n");
        output.push_str(
            &candidate
                .shared_behavior
                .iter()
                .map(|label| format!("`{label}`"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        output.push_str(".\n\n");
    }
    output
}

fn recommendation_name(recommendation: Recommendation) -> &'static str {
    match recommendation {
        Recommendation::ExtractComponent => "extract component",
        Recommendation::ExtractGenericFunction => "extract generic function",
        Recommendation::GenericParameter => "consider generic parameter",
        Recommendation::Merge => "consider merge",
        Recommendation::ReviewPartialState => "review partial state overlap",
        Recommendation::ReviewStructuralOverlap => "review structural overlap",
    }
}

fn substitution_name(substitution: Substitution) -> &'static str {
    match substitution {
        Substitution::Safe => "safe substitution",
        Substitution::Conditional => "conditional substitution",
        Substitution::Incompatible => "structurally related but incompatible",
    }
}

fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Bidirectional => "bidirectional",
        Direction::LeftToRight => "left-to-right",
        Direction::RightToLeft => "right-to-left",
    }
}

fn node_id(abstraction: &AbstractionRef) -> String {
    format!(
        "{}:{}:{}",
        abstraction.path, abstraction.line, abstraction.name
    )
}

fn mermaid_text(value: &str) -> String {
    value.replace('"', "'")
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let json = serde_json::to_string_pretty(value).context("serialize similarity artifact")?;
    write_text(path, &format!("{json}\n"))
}

fn write_text(path: &Path, value: &str) -> Result<()> {
    fs::write(path, value).with_context(|| format!("write similarity artifact: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::similarity::analysis::analyze_source;

    #[test]
    fn writes_revision_scoped_explainable_artifacts() {
        let report = analyze_source(
            "src/lib.rs",
            r#"
                struct Left<T> { values: Vec<T> }
                struct Right<U> { values: VecDeque<U> }
            "#,
        )
        .expect("analysis report");
        let temp = tempfile::tempdir().expect("tempdir");

        let roots = vec!["crates/demo/src".to_owned()];
        let artifacts = write(
            temp.path(),
            "abc123",
            Profile::Advisory,
            &roots,
            false,
            &report,
        )
        .expect("write artifacts");

        assert_eq!(artifacts.document, temp.path().join("report.md"));
        for file in ["report.md", "report.json", "graph.json", "manifest.json"] {
            assert!(temp.path().join(file).is_file(), "missing {file}");
        }
        let markdown = fs::read_to_string(&artifacts.document).expect("read Markdown report");
        assert!(markdown.contains("Left"));
        assert!(markdown.contains("Right"));
        assert!(markdown.contains("Type substitutions"));
        assert!(markdown.contains("```mermaid"));
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(temp.path().join("manifest.json")).expect("read manifest"),
        )
        .expect("manifest JSON");
        assert_eq!(manifest["schema_version"], 2);
        assert_eq!(manifest["roots"][0], "crates/demo/src");
        assert_eq!(manifest["include_default_excluded"], false);
    }
}
