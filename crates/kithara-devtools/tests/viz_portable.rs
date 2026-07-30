use std::{
    fs,
    path::{Path, PathBuf},
};

use clap::{FromArgMatches, Subcommand};
use kithara_devtools::{CoreCommand, Ctx};
use tempfile::tempdir;

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).expect("create fixture directory");
    for entry in fs::read_dir(source).expect("read fixture directory") {
        let entry = entry.expect("fixture entry");
        let destination = target.join(entry.file_name());
        if entry.file_type().expect("fixture type").is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).expect("copy fixture file");
        }
    }
}

fn viz_command(runtime: &str) -> CoreCommand {
    let command = CoreCommand::augment_subcommands(clap::Command::new("xtask"));
    let matches = command
        .try_get_matches_from([
            "xtask",
            "viz",
            "--crate",
            "flow",
            "--lod",
            "3",
            "--semantic",
            "off",
            "--runtime",
            runtime,
        ])
        .expect("parse viz command");
    CoreCommand::from_arg_matches(&matches).expect("build viz command")
}

fn workspace_lod_command(lod: &str) -> CoreCommand {
    workspace_lod_command_with_runtime(lod, "off")
}

fn workspace_lod_command_with_runtime(lod: &str, runtime: &str) -> CoreCommand {
    let command = CoreCommand::augment_subcommands(clap::Command::new("xtask"));
    let matches = command
        .try_get_matches_from([
            "xtask",
            "viz",
            "--lod",
            lod,
            "--semantic",
            "off",
            "--runtime",
            runtime,
        ])
        .expect("parse workspace LOD command");
    CoreCommand::from_arg_matches(&matches).expect("build workspace LOD command")
}

fn filtered_workspace_command() -> CoreCommand {
    let command = CoreCommand::augment_subcommands(clap::Command::new("xtask"));
    let matches = command
        .try_get_matches_from([
            "xtask",
            "viz",
            "--lod",
            "1",
            "--semantic",
            "off",
            "--runtime",
            "off",
            "--exclude-crate",
            "unrelated",
            "--exclude-module",
            "flow::runtime",
        ])
        .expect("parse filtered workspace command");
    CoreCommand::from_arg_matches(&matches).expect("build filtered workspace command")
}

fn crate_lod_command(package: &str, lod: &str) -> CoreCommand {
    let command = CoreCommand::augment_subcommands(clap::Command::new("xtask"));
    let matches = command
        .try_get_matches_from([
            "xtask",
            "viz",
            "--crate",
            package,
            "--lod",
            lod,
            "--semantic",
            "off",
            "--runtime",
            "off",
        ])
        .expect("parse crate LOD command");
    CoreCommand::from_arg_matches(&matches).expect("build crate LOD command")
}

fn auto_lod_command(scope: &[&str]) -> CoreCommand {
    let command = CoreCommand::augment_subcommands(clap::Command::new("xtask"));
    let mut arguments = vec!["xtask", "viz", "--semantic", "off", "--runtime", "off"];
    arguments.extend_from_slice(scope);
    let matches = command
        .try_get_matches_from(arguments)
        .expect("parse automatic LOD command");
    CoreCommand::from_arg_matches(&matches).expect("build automatic LOD command")
}

#[test]
fn workspace_lod_zero_shows_every_crate_without_internal_nodes() {
    let temp = tempdir().expect("tempdir");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture-workspace");
    copy_tree(&fixture, temp.path());
    let manifest = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest).expect("load fixture context");

    kithara_devtools::run(&workspace_lod_command("0"), &ctx).expect("workspace LOD 0 run");

    let output = temp
        .path()
        .join("target/architecture/working-tree/architecture.md");
    let document = fs::read_to_string(&output).expect("architecture document");
    let crate_document = fs::read_to_string(
        output
            .parent()
            .expect("architecture output")
            .join("crates/flow")
            .with_extension("md"),
    )
    .expect("flow crate document");
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("manifest.json")).expect("manifest"),
    )
    .expect("manifest JSON");
    let projection: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("projection.json")).expect("projection"),
    )
    .expect("projection JSON");
    assert!(document.contains("consumer"));
    assert!(document.contains("conditional"));
    assert!(document.contains("flow"));
    assert!(document.contains("support"));
    assert!(document.contains("unrelated"));
    assert!(document.contains("depends on"));
    assert!(!crate_document.contains("depends on"));
    assert!(!crate_document.contains("[\"consumer\"]"));
    assert!(!document.contains("[\"start\"]"));
    assert!(!document.contains("[\"worker\"]"));
    assert!(!document.contains("Arc&lt;"));
    assert_eq!(manifest["lod"], 0);
    assert_eq!(manifest["hidden_nodes"], 0);
    assert!(
        projection["edges"]
            .as_array()
            .expect("projection edges")
            .iter()
            .any(|edge| {
                edge["source"]["package"] == "flow"
                    && edge["target"]["package"] == "conditional"
                    && edge["style"] == "conditional"
            })
    );
}

#[test]
fn focused_crate_hides_workspace_dependencies_and_incoming_neighbors() {
    let temp = tempdir().expect("tempdir");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture-workspace");
    copy_tree(&fixture, temp.path());
    let manifest = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest).expect("load fixture context");

    kithara_devtools::run(&crate_lod_command("flow", "1"), &ctx).expect("crate LOD 1 run");

    let output = temp
        .path()
        .join("target/architecture/working-tree/architecture.md");
    let document = fs::read_to_string(&output).expect("architecture document");
    let projection: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("projection.json")).expect("projection"),
    )
    .expect("projection JSON");
    let metrics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.with_file_name("metrics.json")).expect("metrics"))
            .expect("metrics JSON");

    assert!(!document.contains("depends on"));
    assert!(!document.contains("[\"consumer\"]"));
    assert!(
        projection["nodes"]
            .as_array()
            .expect("projection nodes")
            .iter()
            .all(|node| node["id"]["package"] != "consumer")
    );
    assert!(
        projection["edges"]
            .as_array()
            .expect("projection edges")
            .iter()
            .all(|edge| edge["kind"] != "depends_on")
    );
    assert_eq!(metrics["including_candidates"]["incoming_relations"], 0);
}

#[test]
fn workspace_filters_remove_crates_modules_edges_and_report_findings() {
    let temp = tempdir().expect("tempdir");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture-workspace");
    copy_tree(&fixture, temp.path());
    let manifest_path = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest_path).expect("load fixture context");

    kithara_devtools::run(&filtered_workspace_command(), &ctx).expect("filtered workspace run");

    let output = temp
        .path()
        .join("target/architecture/working-tree/architecture.md");
    let document = fs::read_to_string(&output).expect("architecture document");
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("manifest.json")).expect("manifest"),
    )
    .expect("manifest JSON");
    let projection: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("projection.json")).expect("projection"),
    )
    .expect("projection JSON");
    let metrics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.with_file_name("metrics.json")).expect("metrics"))
            .expect("metrics JSON");
    let graph: serde_json::Value =
        serde_json::from_slice(&fs::read(output.with_file_name("graph.json")).expect("graph"))
            .expect("graph JSON");

    assert!(!document.contains("unrelated"));
    assert!(!document.contains("[\"runtime\"]"));
    assert!(
        projection["nodes"]
            .as_array()
            .expect("projection nodes")
            .iter()
            .all(|node| {
                node["id"]["package"] != "unrelated" && node["id"]["module"] != "runtime"
            })
    );
    assert!(
        projection["edges"]
            .as_array()
            .expect("projection edges")
            .iter()
            .all(|edge| {
                edge["source"]["package"] != "unrelated"
                    && edge["target"]["package"] != "unrelated"
                    && edge["source"]["module"] != "runtime"
                    && edge["target"]["module"] != "runtime"
            })
    );
    assert_eq!(manifest["schema_version"], 4);
    assert_eq!(manifest["lod"], 1);
    assert_eq!(
        manifest["filters"]["exclude_crates"],
        serde_json::json!(["unrelated"])
    );
    assert_eq!(
        manifest["filters"]["exclude_modules"],
        serde_json::json!(["flow::runtime"])
    );
    assert!(
        manifest["filters"]["excluded_nodes"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(
        manifest["filters"]["excluded_edges"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(metrics["contours"].as_object().is_some_and(|contours| {
        contours
            .keys()
            .all(|path| !path.contains("unrelated") && !path.contains("flow/runtime"))
    }));
    assert!(graph["nodes"].as_array().is_some_and(|nodes| {
        nodes
            .iter()
            .any(|node| node["id"]["package"] == "unrelated")
    }));
}

#[test]
fn exclusion_filters_reject_invalid_or_empty_scope() {
    let temp = tempdir().expect("tempdir");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture-workspace");
    copy_tree(&fixture, temp.path());
    let manifest_path = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest_path).expect("load fixture context");

    let selected = kithara_devtools::run(
        &auto_lod_command(&["--crate", "flow", "--exclude-crate", "flow"]),
        &ctx,
    )
    .expect_err("selected excluded crate");
    assert!(selected.to_string().contains("selected workspace package"));

    let selected_module = kithara_devtools::run(
        &auto_lod_command(&[
            "--crate",
            "flow",
            "--module",
            "runtime",
            "--exclude-module",
            "flow::runtime",
        ]),
        &ctx,
    )
    .expect_err("selected excluded module");
    assert!(
        selected_module
            .to_string()
            .contains("selected module scope")
    );

    let unknown = kithara_devtools::run(&auto_lod_command(&["--exclude-crate", "missing"]), &ctx)
        .expect_err("unknown exact excluded crate");
    assert!(
        unknown
            .to_string()
            .contains("workspace package not found for --exclude-crate")
    );

    let empty = kithara_devtools::run(&auto_lod_command(&["--exclude-crate", "*"]), &ctx)
        .expect_err("empty projection");
    assert!(empty.to_string().contains("removed every node"));
}

#[test]
fn automatic_lod_follows_workspace_crate_and_module_scope() {
    let temp = tempdir().expect("tempdir");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture-workspace");
    copy_tree(&fixture, temp.path());
    let manifest_path = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest_path).expect("load fixture context");
    let output = temp
        .path()
        .join("target/architecture/working-tree/manifest.json");

    for (scope, expected) in [
        (&[][..], 0),
        (&["--crate", "flow"][..], 1),
        (&["--crate", "flow", "--module", "runtime"][..], 2),
    ] {
        kithara_devtools::run(&auto_lod_command(scope), &ctx).expect("automatic LOD run");
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&output).expect("manifest")).expect("manifest JSON");
        assert_eq!(manifest["lod"], expected);
    }
}

#[test]
fn crate_lod_one_contracts_modules_into_subsystems() {
    let temp = tempdir().expect("tempdir");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture-workspace");
    copy_tree(&fixture, temp.path());
    let manifest = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest).expect("load fixture context");

    kithara_devtools::run(&crate_lod_command("flow", "1"), &ctx).expect("crate LOD 1 run");

    let output = temp
        .path()
        .join("target/architecture/working-tree/architecture.md");
    let projection: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("projection.json")).expect("projection"),
    )
    .expect("projection JSON");
    let modules = projection["nodes"]
        .as_array()
        .expect("projection nodes")
        .iter()
        .filter(|node| node["id"]["package"] == "flow" && node["kind"] == "module")
        .map(|node| node["id"]["module"].as_str().expect("module"))
        .collect::<Vec<_>>();

    assert_eq!(modules, ["crate", "runtime"]);
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("manifest.json")).expect("manifest"),
    )
    .expect("manifest JSON");
    let metrics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.with_file_name("metrics.json")).expect("metrics"))
            .expect("metrics JSON");
    let subsystem = manifest["partition"]["pages"]
        .as_array()
        .expect("subsystem pages")
        .iter()
        .find(|page| page["label"] == "flow::runtime")
        .expect("runtime subsystem page");
    let subsystem_document = fs::read_to_string(
        output
            .parent()
            .expect("architecture output")
            .join(subsystem["file"].as_str().expect("subsystem page file")),
    )
    .expect("subsystem document");
    assert!(subsystem_document.contains("## Architectural complexity"));
    assert!(subsystem_document.contains("### Metric findings"));
    assert_eq!(metrics["confirmed"]["incoming_relations"], 0);
    assert_eq!(metrics["confirmed"]["outgoing_relations"], 0);
}

#[test]
fn workspace_metrics_are_explainable_and_machine_readable() {
    let temp = tempdir().expect("tempdir");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture-workspace");
    copy_tree(&fixture, temp.path());
    let manifest = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest).expect("load fixture context");

    kithara_devtools::run(&workspace_lod_command("0"), &ctx).expect("workspace metrics run");

    let output = temp
        .path()
        .join("target/architecture/working-tree/metrics.json");
    let metrics: serde_json::Value =
        serde_json::from_slice(&fs::read(output).expect("metrics")).expect("metrics JSON");
    let contours: serde_json::Value = serde_json::from_slice(
        &fs::read(
            temp.path()
                .join("target/architecture/working-tree/contours.json"),
        )
        .expect("contours"),
    )
    .expect("contours JSON");
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(
            temp.path()
                .join("target/architecture/working-tree/manifest.json"),
        )
        .expect("manifest"),
    )
    .expect("manifest JSON");

    assert_eq!(metrics["schema_version"], 1);
    assert_eq!(metrics["scope"]["kind"], "workspace");
    assert_eq!(metrics["confirmed"]["node_count"], 6);
    assert!(
        metrics["confirmed"]["propagation_cost"]
            .as_f64()
            .is_some_and(|value| value > 0.0)
    );
    assert!(
        metrics["architecture_complexity_index"]
            .as_f64()
            .is_some_and(|value| (0.0..=100.0).contains(&value))
    );
    assert!(
        metrics["including_candidates_complexity_index"]
            .as_f64()
            .is_some_and(|value| (0.0..=100.0).contains(&value))
    );
    assert!(
        metrics["contributions"]
            .as_object()
            .is_some_and(|contributions| !contributions.is_empty())
    );
    assert_eq!(contours["schema_version"], 1);
    assert!(
        contours["contours"]
            .as_array()
            .is_some_and(|records| records.iter().any(|record| {
                record["id"]["package"] == "flow"
                    && record["id"]["module"] == "runtime"
                    && record["parent"]["module"] == "crate"
            }))
    );
    assert_eq!(manifest["partition"]["state"], "hierarchical");
    assert_eq!(manifest["files"]["workspace_mermaid"], "workspace.mmd");
    assert!(
        temp.path()
            .join("target/architecture/working-tree/workspace.mmd")
            .is_file()
    );
    assert!(
        temp.path()
            .join("target/architecture/working-tree/crates/flow.mmd")
            .is_file()
    );
    let pages = manifest["partition"]["pages"]
        .as_array()
        .expect("hierarchical pages");
    assert!(pages.iter().any(|page| page["label"] == "flow"));
    let contour_metrics = metrics["contours"].as_object().expect("contour metrics");
    assert!(contour_metrics.contains_key("crates/flow"));
}

#[test]
fn crate_lod_two_groups_methods_into_architectural_abstractions() {
    let temp = tempdir().expect("tempdir");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture-workspace");
    copy_tree(&fixture, temp.path());
    let manifest = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest).expect("load fixture context");

    kithara_devtools::run(&crate_lod_command("flow", "2"), &ctx).expect("crate LOD 2 run");

    let output = temp
        .path()
        .join("target/architecture/working-tree/architecture.md");
    let document = fs::read_to_string(&output).expect("architecture document");
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("manifest.json")).expect("manifest"),
    )
    .expect("manifest JSON");
    let projection: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("projection.json")).expect("projection"),
    )
    .expect("projection JSON");

    assert!(document.contains("[\"Primary\"]"));
    assert!(document.contains("[\"Work\"]"));
    assert!(document.contains("[\"runtime functions\"]"));
    assert!(document.contains("implements"));
    assert!(document.contains("|calls|"));
    assert!(!document.contains("[\"FixtureOnly\"]"));
    assert!(!document.contains("[\"InlineNoise\"]"));
    assert!(!document.contains("[\"testing\"]"));
    assert!(!document.contains("[\"fn start()\"]"));
    assert!(!document.contains("[\"fn worker()\"]"));
    assert!(document.contains("Architecture contours:"));
    assert!(document.contains("abstractions"));
    assert_eq!(manifest["schema_version"], 4);
    assert_eq!(manifest["lod"], 2);
    let lifted_call = projection["edges"]
        .as_array()
        .expect("projection edges")
        .iter()
        .find(|edge| {
            edge["kind"] == "calls"
                && edge["source"]["symbol"]
                    .as_str()
                    .is_some_and(|symbol| symbol.contains("Primary"))
        })
        .expect("lifted call from Primary");
    assert!(
        lifted_call["details"]
            .as_array()
            .expect("call details")
            .iter()
            .any(|detail| {
                detail.as_str().is_some_and(|detail| {
                    detail.contains("Primary::run") && detail.contains("recurse")
                })
            })
    );
    assert!(lifted_call["count"].as_u64().is_some_and(|count| count > 0));
    assert!(
        lifted_call["origins"]
            .as_array()
            .is_some_and(|origins| !origins.is_empty())
    );
}

#[test]
fn crate_lod_three_reveals_constructor_and_boundary_methods() {
    let temp = tempdir().expect("tempdir");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture-workspace");
    copy_tree(&fixture, temp.path());
    let manifest = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest).expect("load fixture context");

    kithara_devtools::run(&crate_lod_command("flow", "3"), &ctx).expect("crate LOD 3 run");

    let output = temp
        .path()
        .join("target/architecture/working-tree/architecture.md");
    let document = fs::read_to_string(&output).expect("architecture document");

    assert!(document.contains("subgraph "));
    assert!(document.contains("[\"fn Primary::new()\"]"));
    assert!(document.contains("[\"fn Primary::run()\"]"));
    assert!(document.contains("[\"fn Work::run()\"]"));
    assert!(document.contains("[\"fn recurse()\"]"));
    assert!(document.contains("[\"fn invoke()\"]"));
    assert!(document.contains("[\"fn worker()\"]"));
    assert!(!document.contains("[\"fn local()\"]"));
}

#[test]
fn crate_lod_one_and_four_bound_the_detail_range() {
    let temp = tempdir().expect("tempdir");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture-workspace");
    copy_tree(&fixture, temp.path());
    let manifest = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest).expect("load fixture context");
    let output = temp
        .path()
        .join("target/architecture/working-tree/architecture.md");

    kithara_devtools::run(&crate_lod_command("flow", "1"), &ctx).expect("crate LOD 1 run");
    let modules = fs::read_to_string(&output).expect("module architecture document");
    assert!(modules.contains("[\"runtime\"]"));
    assert!(!modules.contains("[\"Primary\"]"));
    assert!(!modules.contains("[\"fn start()\"]"));

    kithara_devtools::run(&crate_lod_command("flow", "4"), &ctx).expect("crate LOD 4 run");
    let full = fs::read_to_string(&output).expect("full architecture document");
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("manifest.json")).expect("manifest"),
    )
    .expect("manifest JSON");
    let projection: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("projection.json")).expect("projection"),
    )
    .expect("projection JSON");
    assert_eq!(manifest["partition"]["state"], "partitioned");
    assert_eq!(
        manifest["partition"]["covered_nodes"],
        projection["nodes"]
            .as_array()
            .expect("projection nodes")
            .len()
    );
    let mut rendered = full;
    for page in manifest["partition"]["pages"]
        .as_array()
        .expect("partition pages")
    {
        let page = output
            .parent()
            .expect("architecture output directory")
            .join(page["file"].as_str().expect("page file"));
        assert!(page.is_file());
        rendered.push_str(&fs::read_to_string(page).expect("contour document"));
    }
    assert!(rendered.contains("[\"Primary\"]"));
    assert!(rendered.contains("[\"fn start()\"]"));
    assert!(rendered.contains("[\"fn local()\"]"));
    assert!(rendered.contains("Arc&lt;"));
}

#[test]
fn portable_workspace_produces_stable_mermaid_artifacts() {
    let temp = tempdir().expect("tempdir");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture-workspace");
    copy_tree(&fixture, temp.path());
    let manifest = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest).expect("load fixture context");

    kithara_devtools::run(&viz_command("off"), &ctx).expect("first viz run");
    let output = temp
        .path()
        .join("target/architecture/working-tree/architecture.md");
    let first = fs::read_to_string(&output).expect("architecture document");
    assert!(first.contains("flowchart LR"));
    assert!(first.contains("flow"));
    assert!(!first.contains("consumer"));
    assert!(!first.contains("support"));
    assert!(!first.contains("depends on"));
    assert!(!first.contains("unrelated"));
    assert!(first.contains("start"));
    assert!(first.contains("worker"));
    assert!(first.contains("Arc&lt;"));
    assert!(first.contains("## Limitations"));
    assert!(output.with_file_name("graph.json").is_file());
    assert!(output.with_file_name("manifest.json").is_file());
    let first_metrics = fs::read(output.with_file_name("metrics.json")).expect("first metrics");
    let first_contours = fs::read(output.with_file_name("contours.json")).expect("first contours");

    kithara_devtools::run(&viz_command("off"), &ctx).expect("second viz run");
    let second = fs::read_to_string(output).expect("architecture document");
    assert_eq!(first, second);
    assert_eq!(
        first_metrics,
        fs::read(
            temp.path()
                .join("target/architecture/working-tree/metrics.json")
        )
        .expect("second metrics")
    );
    assert_eq!(
        first_contours,
        fs::read(
            temp.path()
                .join("target/architecture/working-tree/contours.json")
        )
        .expect("second contours")
    );
}

#[test]
fn configured_runtime_scenario_enriches_the_same_graph() {
    let temp = tempdir().expect("tempdir");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture-workspace");
    copy_tree(&fixture, temp.path());
    let manifest = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest).expect("load fixture context");

    kithara_devtools::run(&viz_command("off"), &ctx).expect("static viz run");
    let output = temp
        .path()
        .join("target/architecture/working-tree/architecture.md");
    let static_metrics: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("metrics.json")).expect("static metrics"),
    )
    .expect("static metrics JSON");

    kithara_devtools::run(&viz_command("auto"), &ctx).expect("runtime viz run");

    let document = fs::read_to_string(&output).expect("architecture document");
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("manifest.json")).expect("manifest"),
    )
    .expect("manifest JSON");
    let runtime_metrics: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("metrics.json")).expect("runtime metrics"),
    )
    .expect("runtime metrics JSON");
    assert!(document.contains("classDef observed"));
    assert_eq!(manifest["status"], "runtime-enriched");
    assert_eq!(manifest["runtime"]["scenarios"][0]["state"], "complete");
    assert_eq!(runtime_metrics["confirmed"], static_metrics["confirmed"]);
    assert_eq!(
        runtime_metrics["architecture_complexity_index"],
        static_metrics["architecture_complexity_index"]
    );
    assert_eq!(
        runtime_metrics["including_candidates_complexity_index"],
        static_metrics["including_candidates_complexity_index"]
    );
    assert!(runtime_metrics["runtime"]["observed_relations"].is_number());
    assert!(
        manifest["runtime"]["scenarios"][0]["trace"]["records"]
            .as_u64()
            .is_some_and(|records| records > 0)
    );
    assert!(
        output
            .with_file_name("traces")
            .join("flow-runtime.jsonl")
            .is_file()
    );
}

#[test]
fn excluded_test_harness_can_still_produce_runtime_evidence() {
    let temp = tempdir().expect("tempdir");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture-workspace");
    copy_tree(&fixture, temp.path());
    fs::write(
        temp.path().join(".config/xtask.toml"),
        r#"
[project]
name = "portable-flow"

[architecture.filters]
exclude_crates = ["harness"]

[[architecture.runtime.scenarios]]
name = "harness-runtime"
command = "test"
package = "harness"
test = "architecture"
filter = "writes_architecture_trace"
timeout_secs = 30
"#,
    )
    .expect("filtered runtime config");
    let manifest_path = temp.path().join("Cargo.toml");
    let ctx = Ctx::load_from_manifest(&manifest_path).expect("load fixture context");

    kithara_devtools::run(&workspace_lod_command_with_runtime("3", "auto"), &ctx)
        .expect("runtime evidence from excluded harness");

    let output = temp
        .path()
        .join("target/architecture/working-tree/architecture.md");
    let document = fs::read_to_string(&output).expect("architecture document");
    let graph: serde_json::Value =
        serde_json::from_slice(&fs::read(output.with_file_name("graph.json")).expect("graph"))
            .expect("graph JSON");
    let projection: serde_json::Value = serde_json::from_slice(
        &fs::read(output.with_file_name("projection.json")).expect("projection"),
    )
    .expect("projection JSON");

    assert!(document.contains("classDef observed"));
    assert!(!document.contains("[\"harness\"]"));
    assert!(
        graph["nodes"]
            .as_array()
            .expect("graph nodes")
            .iter()
            .any(|node| node["id"]["package"] == "harness")
    );
    assert!(
        projection["nodes"]
            .as_array()
            .expect("projection nodes")
            .iter()
            .all(|node| node["id"]["package"] != "harness")
    );
}
