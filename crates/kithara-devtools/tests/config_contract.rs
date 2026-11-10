use std::{fs, path::Path};

use kithara_devtools::common::{
    project::{ProjectConfig, TestCargoRunner, TestNextestRunner, TestRunner},
    scope::Scope,
    walker::{relative_to, workspace_rs_files_scoped},
};
use tempfile::tempdir;

fn write_config(root: &Path, text: &str) {
    let config_dir = root.join(".config");
    fs::create_dir_all(&config_dir).expect("create config dir");
    fs::write(config_dir.join("xtask.toml"), text).expect("write config");
}

#[test]
fn missing_config_file_yields_defaults() {
    let temp = tempdir().expect("tempdir");

    let config = ProjectConfig::load(temp.path()).expect("load missing config");

    assert!(config.workspace_scan.exclude.is_empty());
    assert_eq!(config.perf.nextest_profile, "perf");
    assert!(!config.test.no_block.enabled);
    assert!(config.test.no_block.items.is_empty());
    assert!(!config.test.load.enabled);
    assert!(config.test.load.items.is_empty());
}

#[test]
fn unknown_section_is_a_typed_error() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[not_a_real_section]
name = "typo"
"#,
    );

    let error = ProjectConfig::load(temp.path()).expect_err("unknown section fails");
    let message = format!("{error:#}");

    assert!(
        message.contains("not_a_real_section"),
        "error did not mention offending token: {message}"
    );
}

#[test]
fn top_level_consumer_section_is_rejected() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[android]
ffi_crate = "kithara-ffi"
"#,
    );

    let error = ProjectConfig::load(temp.path()).expect_err("top-level consumer section fails");
    let message = format!("{error:#}");

    assert!(
        message.contains("android"),
        "error did not mention offending token: {message}"
    );
}

#[test]
fn ext_table_accepts_consumer_sections() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[ext.android]
ffi_crate = "kithara-ffi"

[ext.local_tool]
enabled = true
"#,
    );

    let config = ProjectConfig::load(temp.path()).expect("load ext passthrough config");

    assert!(config.ext.contains_key("android"));
    assert!(config.ext.contains_key("local_tool"));
}

#[test]
fn workspace_scan_parses() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[workspace-scan]
exclude = ["foo/**"]
"#,
    );

    let config = ProjectConfig::load(temp.path()).expect("load workspace scan config");

    assert_eq!(config.workspace_scan.exclude, ["foo/**"]);
}

#[test]
fn perf_config_parses_generic_lanes() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[perf]
primary_lane = "flash-on-http"
nextest_profile = "suite-perf"
frame_prefix = "demo"

[[perf.lanes]]
flash = true
backend = "http"

[[perf.lanes]]
flash = false
backend = "native"
"#,
    );

    let config = ProjectConfig::load(temp.path()).expect("load perf config");

    assert_eq!(config.perf.primary_lane, "flash-on-http");
    assert_eq!(config.perf.frame_prefix.as_deref(), Some("demo"));
    assert_eq!(config.perf.nextest_profile, "suite-perf");
    assert_eq!(config.perf.lanes.len(), 2);
    assert!(config.perf.lanes[0].flash);
    assert_eq!(config.perf.lanes[0].backend, "http");
}

#[test]
fn test_config_parses_base_features() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[test]
features = ["always-on"]

[test.flash]
features = ["virtual-time"]

[test.no_block]
default = false
features = ["no-block-detector"]

[test.load]
default = false
features = ["cpu-pressure"]
"#,
    );

    let config = ProjectConfig::load(temp.path()).expect("load test config");

    assert_eq!(config.test.features, ["always-on"]);
    assert_eq!(config.test.flash.items, ["virtual-time"]);
    assert!(!config.test.no_block.enabled);
    assert_eq!(config.test.no_block.items, ["no-block-detector"]);
    assert!(!config.test.load.enabled);
    assert_eq!(config.test.load.items, ["cpu-pressure"]);
}

#[test]
fn stress_config_owns_generic_modes_environment_and_artifacts() {
    let temp = tempdir().expect("tempdir");
    let source = r#"
[stress]
default_modes = ["baseline"]
default_tier = "night"
nextest_config = "config/runner.toml"
nextest_profile = "repeated"
default_filter = "all()"
default_count = 3
max_count = 10
test_threads = "4"
max_test_threads = 8
build_dir = "target-stress"
raw_output = "target/evidence"
report_output = "target/report.md"
workflow_job_timeout_minutes = 60

[stress.tiers.night]
lanes = ["workspace"]

[stress.artifacts]
attempts = "attempts.json"
subject_junit = "target/runner/junit.xml"
inventory = "inventory.json"
junit = "junit.xml"
log = "runner.log"
manifest = "manifest.json"
pressure = "pressure.jsonl"
report = "report.md"

[stress.environment]
remove = ["OLD_TRACE"]

[stress.modes.baseline]
flash = true
load = true
test_threads = "1"

[stress.modes.baseline.set_env]
TRACE_LEVEL = "warn"

[stress.modes.baseline.raw_path_env]
CAPTURE_DIR = "captures"

[test]
default_lane = "workspace"
default_backend = "http"

[test.lanes.workspace]
cargo.workspace = true

[test.net_backends.http]
features = []
"#;
    write_config(temp.path(), source);

    let config = ProjectConfig::load(temp.path()).expect("load stress config");
    let mode = config.stress.mode("baseline").expect("configured mode");

    assert_eq!(config.stress.nextest_profile, "repeated");
    assert_eq!(config.stress.max_count, 10);
    assert_eq!(mode.flash, Some(true));
    assert_eq!(mode.load, Some(true));
    assert_eq!(mode.test_threads.as_deref(), Some("1"));
    assert_eq!(mode.set_env["TRACE_LEVEL"], "warn");
    assert_eq!(mode.raw_path_env["CAPTURE_DIR"], "captures");

    for value in ["", "0", "9", "1.5", "many"] {
        write_config(
            temp.path(),
            &source.replace("test_threads = \"1\"", &format!("test_threads = {value:?}")),
        );
        let error = ProjectConfig::load(temp.path()).expect_err("invalid mode thread budget");
        assert!(
            format!("{error:#}").contains("stress.modes.baseline.test_threads"),
            "{error:#}"
        );
    }
    write_config(
        temp.path(),
        &source.replace("test_threads = \"1\"", "test_threads = \"num-cpus\""),
    );
    ProjectConfig::load(temp.path()).expect("mode budget supports the CPU-count selector");
}

/// A command mode cannot apply lane runner settings.
#[test]
fn a_stress_mode_that_runs_a_command_cannot_ask_for_a_toggle() {
    let template = r#"
[stress]
default_modes = ["probe"]
default_tier = "night"
nextest_config = "config/runner.toml"
nextest_profile = "repeated"
default_filter = "all()"
default_count = 1
max_count = 1
test_threads = "1"
max_test_threads = 1
build_dir = "target-stress"
raw_output = "target/evidence"
report_output = "target/report.md"
workflow_job_timeout_minutes = 60

[stress.tiers.night]
lanes = ["workspace"]

[stress.artifacts]
attempts = "attempts.json"
subject_junit = "target/runner/junit.xml"
inventory = "inventory.json"
junit = "junit.xml"
log = "runner.log"
manifest = "manifest.json"
pressure = "pressure.jsonl"
report = "report.md"

[stress.modes.probe]
command = ["probe", "run"]
attempt_junit = "probe/junit.xml"
TOGGLE

[test]
default_lane = "workspace"
default_backend = "http"

[test.lanes.workspace]
cargo.workspace = true

[test.net_backends.http]
features = []
"#;
    for (setting, value) in [
        ("flash", "true"),
        ("flash", "false"),
        ("no_block", "true"),
        ("no_block", "false"),
        ("load", "true"),
        ("load", "false"),
        ("test_threads", "'1'"),
    ] {
        let temp = tempdir().expect("tempdir");
        write_config(
            temp.path(),
            &template.replace("TOGGLE", &format!("{setting} = {value}")),
        );

        let error = ProjectConfig::load(temp.path()).expect_err("a toggled command mode fails");

        assert!(
            format!("{error:#}").contains("runs a command, so its runner settings reach nothing"),
            "{error:#}"
        );
    }
}

#[test]
fn stress_envelope_policy_requires_an_envelope_artifact() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[stress]
default_modes = ["baseline"]
default_tier = "night"
nextest_config = "config/runner.toml"
nextest_profile = "repeated"
default_filter = "all()"
default_count = 1
max_count = 1
test_threads = "1"
max_test_threads = 1
build_dir = "target-stress"
raw_output = "target/evidence"
report_output = "target/report.md"
workflow_job_timeout_minutes = 60

[stress.tiers.night]
lanes = ["workspace"]

[stress.artifacts]
attempts = "attempts.json"
subject_junit = "target/runner/junit.xml"
inventory = "inventory.json"
junit = "junit.xml"
log = "runner.log"
manifest = "manifest.json"
pressure = "pressure.jsonl"
report = "report.md"

[stress.evidence]
envelope_suffix_markers = [" payload="]

[stress.modes.baseline]

[test]
default_lane = "workspace"
default_backend = "http"

[test.lanes.workspace]
cargo.workspace = true

[test.net_backends.http]
features = []
"#,
    );

    let error = ProjectConfig::load(temp.path()).expect_err("orphan envelope policy fails");

    assert!(format!("{error:#}").contains("stress.artifacts.envelope_dir"));
}

/// Left unnamed, the build directory is whatever the host already had in
/// `CARGO_TARGET_DIR` — a directory shared with everything else on that machine,
/// which is how five runs lost a whole lane to binaries cleared under them. The
/// config has to say it rather than fall back to the environment.
#[test]
fn a_stress_config_must_name_the_directory_it_builds_into() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[stress]
default_modes = ["baseline"]
default_tier = "night"
nextest_config = "config/runner.toml"
nextest_profile = "repeated"
default_filter = "all()"
default_count = 1
max_count = 1
test_threads = "1"
max_test_threads = 1
raw_output = "target/evidence"
report_output = "target/report.md"
workflow_job_timeout_minutes = 60

[stress.tiers.night]
lanes = ["workspace"]

[stress.artifacts]
attempts = "attempts.json"
subject_junit = "runner/junit.xml"
inventory = "inventory.json"
junit = "junit.xml"
log = "runner.log"
manifest = "manifest.json"
pressure = "pressure.jsonl"
report = "report.md"

[stress.modes.baseline]

[test]
default_lane = "workspace"
default_backend = "http"

[test.lanes.workspace]
cargo.workspace = true

[test.net_backends.http]
features = []
"#,
    );

    let error = ProjectConfig::load(temp.path()).expect_err("an unnamed build directory fails");

    assert!(format!("{error:#}").contains("stress.build_dir"));
}

#[test]
fn workspace_scan_excludes_walked_files() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[workspace-scan]
exclude = ["crates/generated/**"]
"#,
    );
    let kept_dir = temp.path().join("crates/app/src");
    let excluded_dir = temp.path().join("crates/generated/src");
    fs::create_dir_all(&kept_dir).expect("create kept dir");
    fs::create_dir_all(&excluded_dir).expect("create excluded dir");
    fs::write(kept_dir.join("lib.rs"), "").expect("write kept file");
    fs::write(excluded_dir.join("lib.rs"), "").expect("write excluded file");

    let files =
        workspace_rs_files_scoped(temp.path(), &Scope::default()).expect("walk scoped files");
    let rels: Vec<String> = files
        .iter()
        .map(|path| {
            relative_to(temp.path(), path)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();

    assert_eq!(rels, ["crates/app/src/lib.rs"]);
}

/// A lane names what it builds and what runs it as typed keys, which is what
/// lets every executor and the planner derive the same cargo build from it.
#[test]
fn a_test_lane_is_typed_cargo_options_and_a_runner() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[test.lanes.ui]
cargo.packages = ["demo-ui"]
cargo.profile = "test-release"
cargo.lib = true
cargo.tests = ["frames"]
runner.nextest.filter = "binary(frames)"
runner.nextest.test_threads = 1
runner.nextest.ignore_default_filter = true

[test.lanes.doc]
cargo.workspace = true
cargo.exclude = ["demo-fuzz"]
runner.cargo.doc = true
"#,
    );

    let config = ProjectConfig::load(temp.path()).expect("load typed lanes");

    let ui = &config.test.lanes["ui"];
    assert_eq!(ui.cargo.packages, ["demo-ui"]);
    assert_eq!(ui.cargo.profile.as_deref(), Some("test-release"));
    assert!(ui.cargo.lib);
    assert_eq!(ui.cargo.tests, ["frames"]);
    assert_eq!(
        ui.runner,
        TestRunner::Nextest(TestNextestRunner {
            filter: Some("binary(frames)".to_owned()),
            test_threads: Some(1),
            ignore_default_filter: true,
        })
    );
    let doc = &config.test.lanes["doc"];
    assert!(doc.cargo.workspace);
    assert_eq!(doc.cargo.exclude, ["demo-fuzz"]);
    assert_eq!(
        doc.runner,
        TestRunner::Cargo(TestCargoRunner {
            doc: true,
            ..TestCargoRunner::default()
        })
    );
}

#[test]
fn a_test_lane_has_one_runner() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[test.lanes.both]
cargo.workspace = true
runner.nextest.filter = "all()"
runner.cargo.doc = true
"#,
    );

    let error = ProjectConfig::load(temp.path()).expect_err("two runners fail");

    assert!(
        format!("{error:#}").contains("wanted exactly 1 element"),
        "{error:#}"
    );
}

/// The argv keys are gone: a lane that still spells its command is refused
/// at load instead of running something its typed keys do not say.
#[test]
fn a_test_lane_cannot_spell_its_command() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        r#"
[test.lanes.raw]
cargo.workspace = true
prefix_args = ["nextest", "run", "-E", "all()"]
"#,
    );

    let error = ProjectConfig::load(temp.path()).expect_err("an argv key fails");

    assert!(
        format!("{error:#}").contains("unknown field `prefix_args`"),
        "{error:#}"
    );
}

/// A campaign config whose one tier, `night`, repeats `lanes`, over the test
/// lanes `test_lanes`.
fn campaign(lanes: &str, test_lanes: &str) -> String {
    format!(
        r#"
[stress]
default_modes = ["baseline"]
default_tier = "night"
nextest_config = "config/runner.toml"
nextest_profile = "repeated"
default_filter = "all()"
default_count = 3
max_count = 10
test_threads = "4"
max_test_threads = 8
build_dir = "target-stress"
raw_output = "target/evidence"
report_output = "target/report.md"
workflow_job_timeout_minutes = 60

[stress.tiers.night]
lanes = {lanes}

[stress.artifacts]
attempts = "attempts.json"
subject_junit = "target/runner/junit.xml"
inventory = "inventory.json"
junit = "junit.xml"
log = "runner.log"
manifest = "manifest.json"
pressure = "pressure.jsonl"
report = "report.md"

[stress.modes.baseline]
flash = true

[test]
default_lane = "workspace"
default_backend = "http"

[test.net_backends.http]
features = []

[test.lanes.workspace]
cargo.workspace = true
{test_lanes}
"#
    )
}

fn campaign_error(lanes: &str, test_lanes: &str) -> String {
    let temp = tempdir().expect("tempdir");
    write_config(temp.path(), &campaign(lanes, test_lanes));
    format!(
        "{:#}",
        ProjectConfig::load(temp.path()).expect_err("the campaign is refused")
    )
}

/// Stress hands nextest its own thread count, so a stress lane that names one
/// would pass nextest the flag twice.
#[test]
fn a_stress_lane_cannot_name_its_own_thread_count() {
    let error = campaign_error(
        r#"["workspace", "gpu"]"#,
        "[test.lanes.gpu]\ncargo.workspace = true\nrunner.nextest.test_threads = 1\n",
    );

    assert!(
        error.contains("must run nextest under `stress.test_threads`"),
        "{error}"
    );
}

/// A campaign repeats every lane it names under every lane mode, so each one
/// is named once, is configured, and is run by nextest.
#[test]
fn a_stress_campaign_names_each_lane_it_can_repeat_once() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        &campaign(
            r#"["workspace", "tools"]"#,
            "[test.lanes.tools]\ncargo.packages = [\"demo-tools\"]\n",
        ),
    );
    let config = ProjectConfig::load(temp.path()).expect("a campaign over two lanes");
    assert_eq!(config.stress.tiers["night"].lanes, ["workspace", "tools"]);

    for (lanes, refusal) in [
        ("[]", "stress tier `night` must name at least one lane"),
        (
            r#"["workspace", "workspace"]"#,
            "stress tier `night` names `workspace` twice",
        ),
        (
            r#"["absent"]"#,
            "stress lane `absent` is not configured under test.lanes",
        ),
        (r#"["doc"]"#, "must run nextest"),
    ] {
        let error = campaign_error(
            lanes,
            "[test.lanes.doc]\ncargo.workspace = true\nrunner.cargo.doc = true\n",
        );
        assert!(error.contains(refusal), "{lanes}: {error}");
    }
}

/// A run that names no tier repeats the default one, so the default has to
/// be a tier the project configures.
#[test]
fn the_default_stress_tier_is_a_configured_tier() {
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        &campaign(r#"["workspace"]"#, "")
            .replace(r#"default_tier = "night""#, r#"default_tier = "dawn""#),
    );

    let error = format!(
        "{:#}",
        ProjectConfig::load(temp.path()).expect_err("an unconfigured default tier")
    );

    assert!(
        error.contains("stress.default_tier names `dawn`, which is not configured"),
        "{error}"
    );
}

/// A lane the campaign leaves out says why, so a deliberate gap reads
/// differently from a forgotten one.
#[test]
fn a_stress_exemption_names_a_configured_lane_and_why() {
    let exempt = |entry: &str| {
        format!(
            "[test.lanes.doc]\ncargo.workspace = true\nrunner.cargo.doc = true\n\
             [stress.not_stressed]\n{entry}\n"
        )
    };
    let temp = tempdir().expect("tempdir");
    write_config(
        temp.path(),
        &campaign(r#"["workspace"]"#, &exempt(r#"doc = "doc-tests run once""#)),
    );
    let config = ProjectConfig::load(temp.path()).expect("an exempt lane");
    assert_eq!(config.stress.not_stressed["doc"], "doc-tests run once");

    for (entry, refusal) in [
        (
            r#"absent = "gone""#,
            "stress exemption `absent` is not configured under test.lanes",
        ),
        (
            r#"doc = " ""#,
            "stress.not_stressed reason for `doc` must not be empty",
        ),
        (
            r#"workspace = "repeated anyway""#,
            "stress lane `workspace` is both stressed and exempt",
        ),
    ] {
        let error = campaign_error(r#"["workspace"]"#, &exempt(entry));
        assert!(error.contains(refusal), "{entry}: {error}");
    }
}
