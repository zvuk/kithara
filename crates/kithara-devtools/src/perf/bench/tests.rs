use std::path::{Path, PathBuf};

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
