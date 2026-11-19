use std::path::{Path, PathBuf};

use clap::Parser;
use serde_json::{Value, json};
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
use std::{env, fs::{self, OpenOptions}, io::Write};
fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    let ids = include_str!("../ids");
    if let ["--bench", mode, filter @ ("paired" | "head-only")] = args.as_slice() {
        let mut events = OpenOptions::new().create(true).append(true).open(EVENTS).unwrap();
        writeln!(events, "{mode}:{filter}:{}", env::current_exe().unwrap().display()).unwrap();
        let mut selected: Vec<_> = ids.lines()
            .filter(|id| *filter != "paired" || !id.starts_with("forms/"))
            .collect();
        if *mode == "--test" && selected.contains(&"group/reverse") {
            selected.reverse();
        }
        for id in selected {
            match *mode {
                "--list" => println!("{id}: benchmark"),
                "--test" => {
                    println!("Testing {id}");
                    match id {
                        "group/exit" => std::process::exit(2),
                        "group/no-success" => return,
                        "group/duplicate" => println!("Success\nTesting {id}"),
                        "group/artifact" => {
                            let home = env::var("CRITERION_HOME").unwrap();
                            fs::write(std::path::Path::new(&home).join("sample.json"), "{}").unwrap();
                        }
                        _ => {}
                    }
                    println!("Success");
                }
                _ => std::process::exit(3),
            }
        }
        return;
    }
    if args.iter().any(|arg| matches!(*arg, "paired" | "head-only")) {
        let mut events = OpenOptions::new().create(true).append(true).open(EVENTS).unwrap();
        writeln!(events, "timing").unwrap();
        std::process::exit(4);
    }
    if args.contains(&"--list") {
        for id in ids.lines() { println!("{id}: benchmark"); }
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

// A child that writes to stdout and stderr before exiting, used to prove the
// bench build path streams stderr while keeping stdout as parseable JSON.
#[cfg(unix)]
fn stream_fixture(root: &Path, exit_code: i32) -> PathBuf {
    let source = root.join("stream_child.rs");
    fs::write(
        &source,
        r#"
use std::io::Write;
fn main() {
    if let Some(pid_file) = std::env::args_os().nth(1) {
        std::fs::write(pid_file, std::process::id().to_string()).unwrap();
    }
    std::io::stdout().write_all(b"{\"json\":true}").unwrap();
    std::io::stderr().write_all(b"stderr-before-exit\n").unwrap();
    std::process::exit(EXIT_CODE);
}
"#
        .replace("EXIT_CODE", &exit_code.to_string()),
    )
    .unwrap();
    let binary = root.join("stream_child");
    command(
        root,
        "rustc",
        &[source.to_str().unwrap(), "-o", binary.to_str().unwrap()],
    );
    binary
}

#[cfg(unix)]
#[test]
fn bench_output_streams_stderr_and_keeps_stdout_json() {
    let temp = tempfile::tempdir().unwrap();
    let log_path = temp.path().join("bench-results.txt");
    let mut log = Log(File::create(&log_path).unwrap());
    for exit_code in [0, 1] {
        let child = stream_fixture(temp.path(), exit_code);
        let mut command = Command::new(&child);
        let result = log.output(&mut command, true);
        assert_eq!(
            result.is_ok(),
            exit_code == 0,
            "exit {exit_code} must propagate as failure"
        );
        if let Ok(output) = result {
            // stdout stays intact JSON even though stderr was streamed.
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                r#"{"json":true}"#
            );
        }
    }
    // stderr reached the bench-results log file exactly once per run.
    let log = fs::read_to_string(&log_path).unwrap();
    assert_eq!(log.matches("stderr-before-exit").count(), 2);
}

// A child that fills stdout beyond the pipe buffer while writing a stderr
// marker first. Proves the parent drains both pipes concurrently: a
// sequential stderr-then-stdout reader would block on the full stdout pipe and
// deadlock unless stdout is drained concurrently.
#[cfg(unix)]
fn pipe_pressure_fixture(root: &Path) -> PathBuf {
    let source = root.join("pipe_pressure_child.rs");
    fs::write(
        &source,
        r#"
use std::io::Write;
fn main() {
    std::io::stderr().write_all(b"pressure-stderr-before-exit\n").unwrap();
    let mut stdout = std::io::stdout().lock();
    // 8 MiB > any OS pipe buffer, so the child blocks unless the parent drains
    // stdout concurrently. Line-delimited JSON stays parseable.
    for index in 0..8 * 1024 * 1024 / 13 {
        writeln!(stdout, "{{\"i\":{index}}}").unwrap();
    }
}
"#,
    )
    .unwrap();
    let binary = root.join("pipe_pressure_child");
    command(
        root,
        "rustc",
        &[source.to_str().unwrap(), "-o", binary.to_str().unwrap()],
    );
    binary
}

#[cfg(unix)]
#[test]
fn bench_output_drains_full_stdout_pipe_without_deadlock() {
    let temp = tempfile::tempdir().unwrap();
    let log_path = temp.path().join("bench-results.txt");
    let mut log = Log(File::create(&log_path).unwrap());
    let child = pipe_pressure_fixture(temp.path());
    let mut command = Command::new(&child);
    let output = log.output(&mut command, true).unwrap();
    // stdout is fully drained (more than any OS pipe buffer) and stays
    // line-delimited JSON.
    assert!(
        output.stdout.len() >= 1024 * 1024,
        "expected stdout well beyond the pipe buffer, got {}",
        output.stdout.len()
    );
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        assert!(line.starts_with('{') && line.ends_with('}'), "{line:?}");
    }
    // stderr marker reached the log file even though stdout filled the pipe.
    assert!(
        fs::read_to_string(&log_path)
            .unwrap()
            .contains("pressure-stderr-before-exit")
    );
}

// A read-only file yields EBADF on write, exercising the helper's error path.
// The helper must return the write error while still reaping the child, so the
// child is not left running (its drop neither kills nor reaps).
#[cfg(unix)]
#[test]
fn bench_output_preserves_error_and_reaps_child() {
    let temp = tempfile::tempdir().unwrap();
    let mut read_only = File::open(temp.path()).unwrap();
    let child = stream_fixture(temp.path(), 0);
    let pid_file = temp.path().join("stream-child.pid");
    let mut command = Command::new(&child);
    command.arg(&pid_file);
    let error = stream_output(&mut command, &mut read_only).unwrap_err();
    assert!(
        error.to_string().contains("Bad file descriptor"),
        "expected EBADF, got {error}"
    );
    let pid = fs::read_to_string(pid_file).unwrap();
    let child_state = Command::new("ps")
        .args(["-p", pid.trim(), "-o", "ppid="])
        .output()
        .unwrap();
    assert!(child_state.status.success() || child_state.status.code() == Some(1));
    let parent = String::from_utf8(child_state.stdout).unwrap();
    assert_ne!(parent.trim(), std::process::id().to_string());
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
        execute(&ctx, &head, true, None).unwrap();
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
    let error = execute(&ctx, &request(&base), true, None).unwrap_err();
    assert_eq!(error.to_string(), "start benchmark command");
    assert_eq!(
        fs::read_to_string(temp.path().join("events")).unwrap(),
        "run:base\nrun:head\n"
    );
    let log = fs::read_to_string(ctx.root.join("bench-results.txt")).unwrap();
    assert_builds_before_measurements(&log, 2);
    assert!(log.contains("missing-benchmark-comparison-tool"));
}

fn assert_builds_before_measurements(log: &str, builds: usize) {
    let before_measurement = log.split_once("\"--save-baseline\"").unwrap().0;
    let directories: BTreeSet<_> = before_measurement
        .lines()
        .filter_map(|line| line.split_once("CARGO_TARGET_DIR=\""))
        .map(|(_, path)| path.split('"').next().unwrap())
        .collect();
    assert_eq!(
        directories.len(),
        builds,
        "builds need distinct owned directories"
    );
    assert_eq!(log.matches("\"--no-run\"").count(), builds);
    assert!(!before_measurement.contains("\"-v\""));
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
        let error = execute(
            &ctx,
            &request(if compared { &base } else { "" }),
            true,
            None,
        )
        .unwrap_err();
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
    let error = execute(&ctx, &request, true, None).unwrap_err();
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

fn preflight_request(ctx: &Ctx, base: &str, id: &str) -> (Request, Value) {
    let mut request = request(base);
    request.head = command(&ctx.root, "git", &["rev-parse", "HEAD"]);
    let declaration = json!({
        "head": request.head,
        "baseline": "irrelevant-to-controller",
        "collections": {
            "z-paired": {
                "filter": "paired",
                "expected_base_ids": [id],
                "expected_head_ids": [id],
            },
            "a-forms": {
                "filter": "head-only",
                "expected_head_ids": [id, "forms/b"],
            },
        },
    });
    (request, declaration)
}

#[test]
fn bench_preflight_builds_lists_then_executes_only_untimed_callbacks() {
    let (temp, ctx, base) = fixture("group/a", "forms/b\ngroup/a");
    let (request, declaration) = preflight_request(&ctx, &base, "group/a");
    let raw = format!("{declaration}\n");
    execute(&ctx, &request, false, Some(&raw)).unwrap();

    let events = fs::read_to_string(temp.path().join("events")).unwrap();
    let calls: Vec<_> = events.lines().collect();
    assert_eq!(calls.len(), 6);
    for (actual, expected) in calls.iter().zip([
        "--list:paired:",
        "--list:paired:",
        "--list:head-only:",
        "--test:paired:",
        "--test:paired:",
        "--test:head-only:",
    ]) {
        assert!(actual.starts_with(expected), "{actual}");
    }
    let executable = |index: usize| calls[index].splitn(3, ':').nth(2).unwrap();
    assert_ne!(executable(0), executable(1));
    for index in [2, 4, 5] {
        assert_eq!(executable(1), executable(index));
    }
    assert_eq!(executable(0), executable(3));

    let log = fs::read_to_string(ctx.root.join("bench-results.txt")).unwrap();
    let builds: Vec<_> = log
        .lines()
        .filter(|line| line.contains("\"--no-run\""))
        .collect();
    assert_eq!(builds.len(), 2);
    assert_ne!(builds[0], builds[1]);
    for build in builds {
        assert!(build.contains("CARGO_PROFILE_BENCH_STRIP=\"none\""));
        assert!(build.contains("CARGO_TARGET_DIR="));
        assert!(build.contains("\"-v\""));
    }
    for marker in [
        "--sample-size",
        "--save-baseline",
        "missing-benchmark-comparison-tool",
    ] {
        assert!(!log.contains(marker), "{marker}");
    }
    assert!(log.contains(&sha256(raw.as_bytes())));
    let results = fs::read_dir(ctx.root.join("bench-results"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        fs::read_to_string(results.join("a-forms/head/test.txt")).unwrap(),
        "Testing forms/b\nSuccess\nTesting group/a\nSuccess\n"
    );
    ensure_no_criterion_results(&results).unwrap();
    let proof = fs::read_to_string(results.join("provenance.txt")).unwrap();
    assert!(proof.contains(&format!("parent {base}")));
    assert!(proof.contains("CARGO_PROFILE_BENCH_STRIP=none"));
    for (arm, call) in [("base", 0), ("head", 1)] {
        let binary_dir = results.join(PREFLIGHT_BINARY_DIR);
        let digest = sha256(&fs::read(binary_dir.join(arm)).unwrap());
        let cargo_json = fs::read_to_string(binary_dir.join(format!("{arm}.cargo.json"))).unwrap();
        assert!(cargo_json.lines().any(|line| {
            let entry: Value = serde_json::from_str(line).unwrap();
            entry["reason"] == "compiler-artifact" && entry["executable"] == executable(call)
        }));
        assert!(proof.contains(&format!("{arm} {PREFLIGHT_BINARY_FORMAT} SHA-256 {digest}")));
        assert!(log.contains(&digest));
    }
}

#[test]
fn bench_preflight_rejects_inherited_criterion_debug() {
    if std::env::var_os("CRITERION_DEBUG").is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("bench_preflight_rejects_inherited_criterion_debug")
            .arg("--nocapture")
            .env("CRITERION_DEBUG", "")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("test result: ok. 1 passed;"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (temp, ctx, base) = fixture("group/a", "group/a\nforms/b");
    let (request, declaration) = preflight_request(&ctx, &base, "group/a");
    let actual = execute(&ctx, &request, false, Some(&declaration.to_string())).unwrap_err();
    assert!(actual.to_string().contains("CRITERION_DEBUG"), "{actual:#}");
    assert!(!ctx.root.join("bench-results.txt").exists());
    assert!(!temp.path().join("events").exists());
}

#[test]
fn bench_preflight_rejects_callback_reordering_for_the_same_binary() {
    let (temp, ctx, base) = fixture("group/reverse\ngroup/a", "group/reverse\ngroup/a");
    let (request, mut declaration) = preflight_request(&ctx, &base, "group/a");
    let cases = json!(["group/a", "group/reverse"]);
    declaration["collections"] = json!({"paired": {
        "filter": "paired",
        "expected_base_ids": cases,
        "expected_head_ids": cases,
    }});
    let actual = execute(&ctx, &request, false, Some(&declaration.to_string())).unwrap_err();
    assert!(
        actual.to_string().contains("differ from inventory order"),
        "{actual:#}"
    );
    let events = fs::read_to_string(temp.path().join("events")).unwrap();
    let modes: Vec<_> = events
        .lines()
        .map(|line| line.split_once(':').unwrap().0)
        .collect();
    assert_eq!(modes, ["--list", "--list", "--test"]);
}

#[test]
fn bench_preflight_rejects_invalid_requests_before_children() {
    let (temp, ctx, base) = fixture("group/a", "group/a\nforms/b");
    let (request, declaration) = preflight_request(&ctx, &base, "group/a");
    let raw = declaration.to_string();
    let mut short = request.clone();
    short.base = "abc".into();
    let mut no_base = request.clone();
    no_base.base.clear();
    let mut no_target = request.clone();
    no_target.target.clear();
    let mut filtered = request.clone();
    filtered.filter = "another".into();
    for (input, raw, measure, error) in [
        (&request, raw.as_str(), true, "timing enabled"),
        (&short, &raw, false, "full lowercase"),
        (&no_base, &raw, false, "full lowercase"),
        (&no_target, &raw, false, "bench target must"),
        (&filtered, &raw, false, "filters belong"),
        (&request, "{", false, "parse preflight declaration"),
    ] {
        let actual = execute(&ctx, input, measure, Some(raw)).unwrap_err();
        assert!(actual.to_string().contains(error), "{actual}");
    }

    for (pointer, value, error) in [
        ("/head", json!("different"), "head does not match"),
        ("/collections", json!({}), "no collections"),
        (
            "/collections/z-paired/expected_base_ids",
            json!([]),
            "empty or duplicate",
        ),
        (
            "/collections/z-paired/expected_head_ids",
            json!(["a", "a"]),
            "empty or duplicate",
        ),
        (
            "/collections/z-paired/filter",
            json!("--test"),
            "valid filter",
        ),
    ] {
        let mut invalid = declaration.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        let actual = execute(&ctx, &request, false, Some(&invalid.to_string())).unwrap_err();
        assert!(actual.to_string().contains(error), "{actual}");
    }
    for name in [
        "",
        ".",
        "..",
        "/absolute",
        "../escape",
        "a/b",
        "a\\b",
        "x:\n",
        "elf",
        "Mach-O",
        "PROVENANCE.TXT",
        "provenance.txt",
    ] {
        let mut invalid = declaration.clone();
        invalid["collections"] = json!({(name): declaration["collections"]["z-paired"]});
        let actual = execute(&ctx, &request, false, Some(&invalid.to_string())).unwrap_err();
        assert!(
            actual.to_string().contains("safe path component"),
            "{actual}"
        );
    }
    assert!(!ctx.root.join("bench-results.txt").exists());
    assert!(!temp.path().join("events").exists());
}

#[test]
fn bench_preflight_collection_directories_are_distinct_on_the_host() {
    let (temp, ctx, base) = fixture("group/a", "group/a");
    let (request, mut declaration) = preflight_request(&ctx, &base, "group/a");
    let paired = declaration["collections"]["z-paired"].clone();
    declaration["collections"] = json!({"Paired": paired, "paired": paired});
    fs::create_dir(temp.path().join("CaseProbe")).unwrap();
    let aliases = temp.path().join("caseprobe").exists();
    let outcome = execute(&ctx, &request, false, Some(&declaration.to_string()));
    if aliases {
        let error = outcome.unwrap_err();
        assert!(error.to_string().contains("create preflight collection"));
        assert!(!temp.path().join("events").exists());
        let log = fs::read_to_string(ctx.root.join("bench-results.txt")).unwrap();
        assert!(!log.contains("\"--no-run\""));
    } else {
        outcome.unwrap();
        let events = fs::read_to_string(temp.path().join("events")).unwrap();
        assert_eq!(events.lines().count(), 8);
    }
}

#[test]
fn bench_preflight_rejects_bad_inventory_before_callbacks() {
    for (base_ids, head_ids, error) in [
        (
            "group/a",
            "group/a\nforms/b\nforms/extra",
            "Criterion IDs do not match",
        ),
        ("group/a", "group/a", "Criterion IDs do not match"),
        (
            "group/a",
            "group/a\ngroup/a\nforms/b",
            "duplicate Criterion ID",
        ),
        ("group/b", "group/a\nforms/b", "Criterion IDs do not match"),
    ] {
        let (temp, ctx, base) = fixture(base_ids, head_ids);
        let (request, declaration) = preflight_request(&ctx, &base, "group/a");
        let actual = execute(&ctx, &request, false, Some(&declaration.to_string())).unwrap_err();
        assert!(format!("{actual:#}").contains(error), "{actual:#}");
        let events = fs::read_to_string(temp.path().join("events")).unwrap();
        assert!(
            events.lines().all(|line| line.starts_with("--list:")),
            "{events}"
        );
    }
}

#[test]
fn bench_preflight_requires_successful_callbacks_without_criterion_files() {
    for (id, error) in [
        ("group/exit", "benchmark command"),
        ("group/no-success", "did not report Success"),
        ("group/duplicate", "duplicate Criterion callback"),
        ("group/artifact", "unexpected Criterion output"),
    ] {
        let (temp, ctx, base) = fixture(id, &format!("{id}\nforms/b"));
        let (request, declaration) = preflight_request(&ctx, &base, id);
        let actual = execute(&ctx, &request, false, Some(&declaration.to_string())).unwrap_err();
        assert!(format!("{actual:#}").contains(error), "{actual:#}");
        let events = fs::read_to_string(temp.path().join("events")).unwrap();
        assert_eq!(
            events
                .lines()
                .filter(|line| line.starts_with("--list:"))
                .count(),
            3
        );
        assert_eq!(
            events
                .lines()
                .filter(|line| line.starts_with("--test:"))
                .count(),
            1
        );
        let log = fs::read_to_string(ctx.root.join("bench-results.txt")).unwrap();
        assert!(!log.contains("Untimed benchmark preflight evidence:"));
    }
}

#[test]
fn bench_preflight_callback_parser_accepts_diagnostics_between_records() {
    let recorded = concat!(
        "crossover_geometry rate=48000 channels=2 bands=3 frequencies=[249.70744, 4325.059] backend=portable target=x86_64\n",
        "Testing crossover_df1_svf/df1_3band/128\n",
        "Success\n",
        "crossover_delta bands=3 block=512 candidate=svf case=retirement_bits moving=true finite_pairs=8 max_abs_finite=1.0141204801825835e31 bit_differences=5 nonfinite_df1=16398 nonfinite_candidate=16398 subnormal_df1=0 subnormal_candidate=0 last_nonzero_df1=Some(8202) last_nonzero_candidate=Some(8202)\n",
        "Testing crossover_df1_svf/df1_3band/512\n",
        "Success\n",
    );
    assert_eq!(
        callback_ids(recorded.as_bytes()).unwrap(),
        [
            "crossover_df1_svf/df1_3band/128",
            "crossover_df1_svf/df1_3band/512",
        ],
    );
}

#[test]
fn bench_preflight_callback_parser_rejects_malformed_records() {
    for (malformed, expected) in [
        ("diagnostic only\n", "no Criterion callbacks were reported"),
        ("Success\n", "unexpected Criterion callback record"),
        ("Testing\n", "unexpected Criterion callback record"),
        (
            "Testing\tcase\nSuccess\n",
            "unexpected Criterion callback record",
        ),
        ("Testing \nSuccess\n", "Criterion callback needs an ID"),
        (
            "Testing case\ndiagnostic\nSuccess\n",
            "did not report Success",
        ),
        (
            "Testing case\nSuccess\nSuccess extra\n",
            "unexpected Criterion callback record",
        ),
        (
            "Testing case\nSuccess\nTesting case\nSuccess\n",
            "duplicate Criterion callback",
        ),
    ] {
        let actual = callback_ids(malformed.as_bytes()).unwrap_err();
        assert!(
            actual.to_string().contains(expected),
            "{malformed:?}: {actual:#}"
        );
    }
}
