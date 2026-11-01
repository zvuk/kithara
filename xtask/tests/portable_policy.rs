//! Kithara policy contracts belong to the workspace that declares them.

use std::{fs, path::Path, process::Command};

use clap::Parser;
use kithara_devtools::{CoreCommand, Ctx, common::project::ProjectConfig, verdict::NotClean};
use serde_json::Value;
use tempfile::{TempDir, tempdir};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: CoreCommand,
}

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
}

fn config(namespace: &str) -> toml::Value {
    let path = root()
        .join(".config")
        .join(namespace)
        .join("thresholds.toml");
    toml::from_str(&fs::read_to_string(path).expect("workspace policy")).expect("valid policy")
}

fn strings(value: &toml::Value) -> Vec<&str> {
    value
        .as_array()
        .expect("policy list")
        .iter()
        .map(|v| v.as_str().expect("policy subject"))
        .collect()
}

#[test]
fn kithara_ownership_policy_keeps_its_declared_subjects() {
    let arch = config("arch");
    for (section, field, expected) in [
        (
            "cancel_root_sites",
            "patterns",
            vec!["CancelToken::root", "CancelToken::never"],
        ),
        (
            "smoothing_primitive_sites",
            "patterns",
            vec!["SmoothingFilter", "one_pole", "OnePole", "smoothing_coeff"],
        ),
        (
            "firewheel_dsp_facade",
            "modules",
            vec![
                "dsp::fade",
                "dsp::mix",
                "dsp::filter::smoothing_filter",
                "param::smoother",
            ],
        ),
        (
            "tokio_dep_quarantine",
            "quarantined",
            vec!["tokio", "tokio-util", "tokio-stream"],
        ),
        (
            "platform_layer_hygiene",
            "arc_owner_files",
            vec![
                "crates/kithara-platform/src/system/ownership.rs",
                "crates/kithara-platform/src/wasm/sync/mod.rs",
            ],
        ),
        (
            "platform_layer_hygiene",
            "excluded_subtrees",
            vec!["backend/", "system/", "loom/", "wasm/"],
        ),
        (
            "platform_layer_hygiene",
            "impl_subtrees",
            vec![
                "flash/sync/",
                "flash/tokio/",
                "flash/system/",
                "common/cancel/",
                "common/time.rs",
            ],
        ),
    ] {
        assert_eq!(
            strings(&arch[section][field]),
            expected,
            "{section}.{field}"
        );
    }
    assert_eq!(
        arch["platform_layer_hygiene"]["enabled"].as_bool(),
        Some(true)
    );
    assert_eq!(
        arch["platform_layer_hygiene"]["root"].as_str(),
        Some("crates/kithara-platform/src/")
    );
    let idioms = config("idioms");
    assert_eq!(idioms["derivable_event"]["enabled"].as_bool(), Some(true));
    assert_eq!(
        idioms["derivable_event"]["owner_path"].as_str(),
        Some("crates/kithara-events")
    );
    for derive in [
        "built_default",
        "control",
        "control_painter",
        "enum_str",
        "into_probe_arg",
        "mirror",
        "patch",
        "phase",
        "ranged",
        "skin_walk",
        "variants",
    ] {
        let key = format!("derivable_{derive}");
        let section = &idioms[key.as_str()];
        assert_eq!(section["enabled"].as_bool(), Some(true), "{derive}");
        assert_eq!(section["severity"].as_str(), Some("deny"), "{derive}");
    }
}

fn target_fixture() -> TempDir {
    let directory = tempdir().expect("policy fixture");
    fs::create_dir_all(directory.path().join(".config/ast-grep")).expect("rule directory");
    fs::copy(
        root().join(".config/ast-grep/arch.no-target-os-outside-platform.yml"),
        directory
            .path()
            .join(".config/ast-grep/arch.no-target-os-outside-platform.yml"),
    )
    .expect("workspace target rule");
    fs::write(
        directory.path().join("sgconfig.yml"),
        "ruleDirs: [.config/ast-grep]\n",
    )
    .expect("fixture config");
    directory
}

fn write_source(root: &Path, path: &str, source: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().expect("source parent")).expect("source directory");
    fs::write(path, source).expect("policy source");
}

fn target_scan(root: &Path, path: &str) -> Vec<Value> {
    let output = Command::new("ast-grep")
        .current_dir(root)
        .args([
            "scan",
            "--config",
            "sgconfig.yml",
            "--include-metadata",
            "--json=stream",
            path,
        ])
        .output()
        .expect("scan workspace target policy");
    assert!(
        matches!(output.status.code(), Some(0 | 1)),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("diagnostics")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("structured diagnostic"))
        .collect()
}

#[test]
fn target_gate_rule_allows_only_existing_structural_abi_items() {
    for path in [
        "crates/kithara-android/src/lib.rs",
        "crates/kithara-ffi/src/lib.rs",
        "crates/kithara-ffi/src/native/mod.rs",
    ] {
        let fixture = target_fixture();
        let source = fs::read_to_string(root().join(path)).expect("workspace ABI source");
        write_source(fixture.path(), path, &source);
        assert!(target_scan(fixture.path(), path).is_empty(), "{path}");
        let business_gate =
            format!("{source}\n#[cfg(not(target_arch = \"wasm32\"))]\nfn gate() {{}}");
        write_source(fixture.path(), path, &business_gate);
        assert_eq!(target_scan(fixture.path(), path).len(), 1, "{path}");
        let foreign = "crates/kithara-audio/src/lib.rs";
        write_source(fixture.path(), foreign, &source);
        assert!(!target_scan(fixture.path(), foreign).is_empty(), "{path}");
    }
}

#[test]
fn target_gate_config_scan_preserves_one_category_and_its_severity() {
    let fixture = target_fixture();
    for path in [
        "crates/kithara-audio/src/lib.rs",
        "crates/kithara-android/src/lib.rs",
        "crates/kithara-ffi/src/lib.rs",
        "crates/kithara-ffi/src/native/mod.rs",
    ] {
        write_source(
            fixture.path(),
            path,
            "#[cfg(target_os = \"android\")] fn gate() {}",
        );
    }
    let diagnostics = target_scan(fixture.path(), "crates");
    assert_eq!(diagnostics.len(), 4);
    for diagnostic in diagnostics {
        let category = diagnostic["metadata"]["category"]
            .as_str()
            .unwrap_or_else(|| diagnostic["ruleId"].as_str().expect("rule id"));
        assert_eq!(category, "arch.no-target-os-outside-platform");
        assert_eq!(diagnostic["severity"], "warning");
    }
    let mut ctx = Ctx::new(fixture.path().to_path_buf(), ProjectConfig::default());
    let ordinary = Cli::parse_from(["policy", "ast-grep"]);
    kithara_devtools::run(&ordinary.command, &ctx).expect("warnings are advisory");
    let strict = Cli::parse_from(["policy", "ast-grep", "--strict"]);
    for scan_all in [false, true] {
        if scan_all {
            ctx.config
                .lint_exclude
                .scan_all_rules
                .push("arch.no-target-os-outside-platform".to_owned());
        }
        let error = kithara_devtools::run(&strict.command, &ctx)
            .expect_err("strict warnings remain findings");
        assert_eq!(
            error
                .downcast_ref::<NotClean>()
                .expect("lint verdict")
                .findings,
            Some(4),
            "scan_all: {scan_all}"
        );
    }
}
