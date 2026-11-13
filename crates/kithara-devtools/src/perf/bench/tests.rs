use std::path::Path;

use clap::Parser;
use tempfile::TempDir;

use super::*;

fn command(root: &Path, program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn fixture(base_ids: &str, head_ids: &str) -> (TempDir, Ctx, String) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repository");
    fs::create_dir_all(root.join("benches")).unwrap();
    fs::create_dir(root.join(".config")).unwrap();
    let events = format!("{:?}", temp.path().join("events"));
    fs::write(
        root.join("Cargo.toml"),
        format!(
            r#"
[workspace]
[workspace.package]
repository = {repository:?}
[package]
name = "fixture-bench"
version = "0.1.0"
edition = "2024"
[features]
spectrum = []
portable = []
[[bench]]
name = "kernel"
harness = false
required-features = ["spectrum", "portable"]
"#,
            repository = root.to_str().unwrap()
        ),
    )
    .unwrap();
    fs::write(
        root.join("benches/kernel.rs"),
        r#"
use std::{env, fs::OpenOptions, io::Write};
fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--list") {
        for id in include_str!("../ids").lines() { println!("{id}: benchmark"); }
        return;
    }
    let mut events = OpenOptions::new().create(true).append(true).open(EVENTS).unwrap();
    writeln!(events, "run:{}", include_str!("../arm").trim()).unwrap();
    println!("{args:?}");
}
"#
        .replace("EVENTS", &events),
    )
    .unwrap();
    let config = "[tools.critcmp]\nprogram = 'missing-benchmark-comparison-tool'\n";
    fs::write(root.join(".config/xtask.toml"), config).unwrap();
    fs::write(root.join("arm"), "base").unwrap();
    fs::write(root.join("ids"), base_ids).unwrap();
    command(&root, "cargo", &["generate-lockfile", "--offline"]);
    command(&root, "git", &["init", "--quiet"]);
    command(&root, "git", &["config", "user.name", "bench fixture"]);
    command(
        &root,
        "git",
        &["config", "user.email", "bench@example.invalid"],
    );
    command(&root, "git", &["add", "."]);
    command(
        &root,
        "git",
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--quiet",
            "-m",
            "base",
        ],
    );
    let base = command(&root, "git", &["rev-parse", "HEAD"]);
    fs::write(root.join("arm"), "head").unwrap();
    fs::write(root.join("ids"), head_ids).unwrap();
    command(&root, "git", &["add", "."]);
    command(
        &root,
        "git",
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--quiet",
            "-m",
            "head",
        ],
    );
    let ctx = Ctx::load_from_manifest(&root.join("Cargo.toml")).unwrap();
    (temp, ctx, base)
}

fn request(base: &str) -> Request {
    Request {
        target: "fixture-bench:kernel:portable,spectrum".into(),
        base: base.into(),
        ..Request::default()
    }
}

#[test]
fn bench_metadata_owns_targets_and_required_features() {
    let (_temp, ctx, _) = fixture("group/a", "group/a");
    let metadata = ctx.metadata().unwrap();
    assert_eq!(
        targets(metadata).iter().map(Target::id).collect::<Vec<_>>(),
        ["fixture-bench:kernel:portable,spectrum"]
    );
    assert_eq!(
        select(metadata, "fixture-bench:kernel:spectrum,portable").unwrap(),
        targets(metadata)
    );
    for invalid in [
        "fixture-bench",
        "other:kernel",
        "fixture-bench:kernel:spectrum",
        "fixture-bench:kernel:unknown",
    ] {
        assert!(select(metadata, invalid).is_err(), "{invalid}");
    }
}

#[test]
fn bench_builds_precede_measurements_and_head_only_needs_no_critcmp() {
    let (temp, ctx, base) = fixture("group/a", "group/a");
    let filter = "group/.*(a|b)$; $(false)";
    let mut head = request("");
    head.filter = filter.into();
    for _ in 0..2 {
        fs::write(temp.path().join("events"), "").unwrap();
        execute(&ctx, &head, true).unwrap();
        assert_eq!(
            fs::read_to_string(temp.path().join("events")).unwrap(),
            "run:head\n"
        );
        let log = fs::read_to_string(ctx.root.join("bench-results.txt")).unwrap();
        assert!(!log.contains("missing-benchmark-comparison-tool"));
        assert!(log.contains(&format!("{:?}", filter)));
        assert_builds_before_measurements(&log, 1);
    }

    fs::write(temp.path().join("events"), "").unwrap();
    let error = execute(&ctx, &request(&base), true).unwrap_err();
    assert_eq!(error.to_string(), "start benchmark command");
    assert_eq!(
        fs::read_to_string(temp.path().join("events")).unwrap(),
        "run:base\nrun:head\n"
    );
    let log = fs::read_to_string(ctx.root.join("bench-results.txt")).unwrap();
    assert_builds_before_measurements(&log, 2);
    assert!(
        fs::read_to_string(ctx.root.join("bench-results.txt"))
            .unwrap()
            .contains("missing-benchmark-comparison-tool")
    );
}

fn assert_builds_before_measurements(log: &str, builds: usize) {
    let before_measurement = log.split_once("\"--save-baseline\"").unwrap().0;
    assert_eq!(log.matches("\"--no-run\"").count(), builds);
    assert_eq!(before_measurement.matches("\"--no-run\"").count(), builds);
}

#[test]
fn bench_refuses_empty_or_unpaired_ids_before_measurement() {
    for (base_ids, head_ids, compared) in [
        ("group/a", "", false),
        ("", "group/a", true),
        ("group/a", "group/b", true),
    ] {
        let (temp, ctx, base) = fixture(base_ids, head_ids);
        let error = execute(&ctx, &request(if compared { &base } else { "" }), true).unwrap_err();
        if base_ids == "group/a" && head_ids == "group/b" {
            assert_eq!(
                error.to_string(),
                "base/head Criterion IDs do not match; only in base: [\"group/a\"]; only in head: [\"group/b\"]"
            );
        }
        assert!(!temp.path().join("events").exists());
        assert!(
            !fs::read_to_string(ctx.root.join("bench-results.txt"))
                .unwrap()
                .contains("missing-benchmark-comparison-tool")
        );
    }
}

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    args: BenchArgs,
}

#[test]
fn bench_target_admission_explains_manifest_feature_differences() {
    let (temp, ctx, base) = fixture("group/a", "group/a");
    let manifest = ctx.root.join("Cargo.toml");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest).unwrap().replace(
            "required-features = [\"spectrum\", \"portable\"]",
            "required-features = [\"portable\"]",
        ),
    )
    .unwrap();
    let mut request = request(&base);
    request.target = "fixture-bench:kernel".into();
    let error = execute(&ctx, &request, true).unwrap_err();
    assert_eq!(
        error.to_string(),
        "base/head benchmark targets or required features differ; only in base: [\"fixture-bench:kernel:portable,spectrum\"]; only in head: [\"fixture-bench:kernel:portable\"]"
    );
    assert!(!temp.path().join("events").exists());
    assert!(
        !fs::read_to_string(ctx.root.join("bench-results.txt"))
            .unwrap()
            .contains("\"--no-run\"")
    );
}

#[test]
fn bench_cli_preserves_all_four_parameters() {
    let cli = Cli::try_parse_from([
        "bench",
        "run",
        "--bench-target",
        "p:b:f",
        "--bench-filter",
        "group/(a|b)$",
        "--bench-ref",
        "feature/branch",
        "--bench-base-ref",
        "main",
    ])
    .unwrap();
    let BenchCommand::Run(request) = cli.args.command else {
        panic!("run command")
    };
    assert_eq!(
        (
            request.target.as_str(),
            request.filter.as_str(),
            request.head.as_str(),
            request.base.as_str()
        ),
        ("p:b:f", "group/(a|b)$", "feature/branch", "main")
    );
    assert_eq!(
        request.parameters(),
        [
            ("BENCH_TARGET", "p:b:f"),
            ("BENCH_FILTER", "group/(a|b)$"),
            ("BENCH_REF", "feature/branch"),
            ("BENCH_BASE_REF", "main")
        ]
    );
}
