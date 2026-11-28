use std::{
    io::Write,
    process::{Command, Output, Stdio},
};

use axum::{Router, routing::get};
use kithara::{
    analysis::TrackAnalysis,
    platform::{
        time::{Duration, timeout},
        tokio::{
            sync::{mpsc, watch},
            task,
        },
    },
};
use kithara_test_fixtures::assets;
use kithara_test_utils::{TestHttpServer, TestTempDir};
use serde_json::{Value, json};

fn invoke(options: &[&str], input: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kithara-analyze"));
    command
        .args(options)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("binary");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("input");
    child.wait_with_output().expect("output")
}

fn input() -> String {
    let path = assets::sine_wav_a440_2s().path().expect("fixture");
    [
        json!({"id":"one","src":path}),
        json!({"id":"two","src":path}),
    ]
    .into_iter()
    .map(|record| record.to_string())
    .collect::<Vec<_>>()
    .join("\n")
}

fn records(lines: &[Value]) -> String {
    lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn file_names(directory: &TestTempDir) -> Vec<String> {
    let mut files: Vec<_> = std::fs::read_dir(directory.path())
        .expect("directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .into_string()
                .expect("UTF-8 name")
        })
        .collect();
    files.sort();
    files
}

fn document(directory: &TestTempDir, id: &str) -> TrackAnalysis {
    let bytes =
        std::fs::read(directory.path().join(format!("{id}.analysis"))).expect("binary record");
    TrackAnalysis::try_from(bytes.as_slice()).expect("an analysis document")
}

/// The stderr line that names `id`.
fn diagnostic(output: &Output, id: &str) -> String {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .find(|line| line.contains(&format!("id={id}")))
        .unwrap_or_else(|| {
            panic!(
                "no diagnostic names {id}: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
        .to_owned()
}

fn output_names_alias(directory: &TestTempDir, first: &str, second: &str) -> bool {
    let first_path = directory.path().join(format!("{first}.json"));
    let second_path = directory.path().join(format!("{second}.json"));
    std::fs::write(&first_path, b"marker").expect("first marker");
    let aliases = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&second_path)
    {
        Ok(file) => {
            drop(file);
            std::fs::remove_file(&second_path).expect("second marker cleanup");
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => true,
        Err(error) => panic!("output alias probe failed: {error}"),
    };
    std::fs::remove_file(&first_path).expect("first marker cleanup");
    aliases
}

#[kithara::test(native, flash(false))]
fn stdout_is_exactly_one_complete_json_line_per_track() {
    let output = invoke(&["--jobs", "2", "-"], &input());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).expect("utf8");
    let records: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON line"))
        .collect();
    assert_eq!(records.len(), 2);
    for record in records {
        assert_eq!(record["status"], "complete");
        assert!(record["waveform"].is_object());
        assert!(record["key"].is_object());
    }
}

#[kithara::test(native, flash(false))]
fn directory_contains_only_complete_named_records() {
    let directory = TestTempDir::new();
    let output = invoke(
        &[
            "--artifacts",
            "waveform",
            "--out",
            directory.path().to_str().expect("path"),
            "-",
        ],
        &input(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let mut files: Vec<_> = std::fs::read_dir(directory.path())
        .expect("directory")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    files.sort();
    assert_eq!(files, ["one.json", "two.json"]);
    for id in ["one", "two"] {
        let bytes = std::fs::read(directory.path().join(format!("{id}.json"))).expect("record");
        let record: Value = serde_json::from_slice(&bytes).expect("complete JSON");
        assert_eq!(record["id"], id);
        assert_eq!(record["status"], "complete");
    }
}

#[kithara::test(native, flash(false))]
fn output_aliases_never_silently_replace_records() {
    let path = assets::sine_wav_a440_2s().path().expect("fixture");
    for (first, second) in [("Track", "track"), ("caf\u{00e9}", "cafe\u{0301}")] {
        let directory = TestTempDir::new();
        let aliases = output_names_alias(&directory, first, second);
        let input = [
            json!({"id":first,"src":path}),
            json!({"id":second,"src":path}),
        ]
        .into_iter()
        .map(|record| record.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        let output = invoke(
            &[
                "--jobs",
                "1",
                "--artifacts",
                "waveform",
                "--out",
                directory.path().to_str().expect("path"),
                "-",
            ],
            &input,
        );
        assert!(output.stdout.is_empty());
        let files: Vec<_> = std::fs::read_dir(directory.path())
            .expect("directory")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        if aliases {
            let first_bytes = std::fs::read(directory.path().join(format!("{first}.json")))
                .expect("first output record");
            let second_bytes = std::fs::read(directory.path().join(format!("{second}.json")))
                .expect("second output record");
            let record: Value = serde_json::from_slice(&first_bytes).expect("complete JSON");
            assert_eq!(files.len(), 1);
            assert_eq!(first_bytes, second_bytes);
            let surviving_id = record["id"].as_str().expect("record ID");
            assert!([first, second].contains(&surviving_id));
            assert_eq!(record["status"], "complete");
            assert_eq!(output.status.code(), Some(1));
        } else {
            assert_eq!(output.status.code(), Some(0));
            assert_eq!(files.len(), 2);
            for id in [first, second] {
                let bytes = std::fs::read(directory.path().join(format!("{id}.json")))
                    .expect("output record");
                let record: Value = serde_json::from_slice(&bytes).expect("complete JSON");
                assert_eq!(record["id"], id);
                assert_eq!(record["status"], "complete");
            }
        }
    }
}

#[kithara::test(native, flash(false))]
fn existing_output_is_not_replaced() {
    let directory = TestTempDir::new();
    let output_path = directory.path().join("one.json");
    std::fs::write(&output_path, b"original").expect("existing record");
    let path = assets::sine_wav_a440_2s().path().expect("fixture");
    let input = json!({"id":"one","src":path}).to_string();
    let output = invoke(
        &[
            "--jobs",
            "1",
            "--artifacts",
            "waveform",
            "--out",
            directory.path().to_str().expect("path"),
            "-",
        ],
        &input,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read(&output_path).expect("existing record"),
        b"original".to_vec()
    );
    let files: Vec<_> = std::fs::read_dir(directory.path())
        .expect("directory")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(files, ["one.json"]);
}

#[kithara::test(native, flash(false))]
fn failed_source_is_a_record_and_a_failure_exit() {
    let path = assets::sine_wav_a440_2s().path().expect("fixture");
    let input = format!(
        "{}\n{}",
        json!({"id":"missing","src":path.with_file_name("absent-export-cli.wav")}),
        json!({"id":"good","src":path})
    );
    let output = invoke(&["--artifacts", "waveform", "-"], &input);
    assert_eq!(output.status.code(), Some(1));
    let records: Vec<Value> = String::from_utf8(output.stdout)
        .expect("utf8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON"))
        .collect();
    assert_eq!(records.len(), 2);
    let failed = records
        .iter()
        .find(|record| record["id"] == "missing")
        .expect("failure record");
    assert_eq!(failed["status"], "failed");
    assert!(!failed["error"].as_str().expect("cause").is_empty());
    assert_eq!(
        records
            .iter()
            .find(|record| record["id"] == "good")
            .expect("good record")["status"],
        "complete"
    );
}

#[kithara::test(native, flash(false))]
fn usage_errors_write_no_records_and_do_not_create_the_output_directory() {
    let directory = TestTempDir::new();
    let out = directory.path().join("not-created");
    for input in [
        "not json",
        "{\"id\":\"same\",\"src\":\"one.wav\"}\n{\"id\":\"same\",\"src\":\"two.wav\"}",
    ] {
        let output = invoke(&["--out", out.to_str().expect("path"), "-"], input);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!out.exists());
    }
    let output = invoke(&["--format", "binary", "one.wav"], "");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[kithara::test(native, flash(false))]
fn binary_records_are_analysis_documents_with_every_selected_artifact() {
    let rhythm = assets::rhythm_wav_deck_a_120bpm_48k()
        .path()
        .expect("fixture");
    let sine = assets::sine_wav_a440_2s().path().expect("fixture");
    let directory = TestTempDir::new();
    let output = invoke(
        &[
            "--format",
            "binary",
            "--out",
            directory.path().to_str().expect("path"),
            "-",
        ],
        &records(&[
            json!({"id":"one","src":rhythm}),
            json!({"id":"two","src":sine}),
        ]),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(file_names(&directory), ["one.analysis", "two.analysis"]);

    let one = document(&directory, "one");
    assert_eq!(one.token().as_str(), "one");
    assert!(one.is_complete());
    assert!(one.waveform().is_some());
    assert!(one.beat().is_some());
    assert!(one.key().is_some());
    assert!(one.fingerprint().waveform().is_some());
    assert!(one.fingerprint().beat().is_some());
    assert!(one.fingerprint().key().is_some());
    assert_eq!(document(&directory, "two").token().as_str(), "two");
}

#[kithara::test(native, flash(false))]
fn a_beat_and_key_binary_record_has_no_waveform() {
    let rhythm = assets::rhythm_wav_deck_a_120bpm_48k()
        .path()
        .expect("fixture");
    let directory = TestTempDir::new();
    let output = invoke(
        &[
            "--artifacts",
            "beat,key",
            "--format",
            "binary",
            "--out",
            directory.path().to_str().expect("path"),
            "-",
        ],
        &records(&[json!({"id":"one","src":rhythm})]),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let one = document(&directory, "one");
    assert!(one.waveform().is_none());
    assert_eq!(one.fingerprint().waveform(), None);
    assert!(one.beat().is_some());
    assert!(one.key().is_some());
}

#[kithara::test(native, flash(false))]
fn a_missing_binary_source_writes_no_file_and_is_named_on_stderr() {
    let path = assets::sine_wav_a440_2s().path().expect("fixture");
    let directory = TestTempDir::new();
    let output = invoke(
        &[
            "--jobs",
            "1",
            "--artifacts",
            "waveform",
            "--format",
            "binary",
            "--out",
            directory.path().to_str().expect("path"),
            "-",
        ],
        &records(&[
            json!({"id":"one","src":path}),
            json!({"id":"two","src":path.with_file_name("absent-export-binary.wav")}),
            json!({"id":"three","src":path}),
        ]),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(file_names(&directory), ["one.analysis", "three.analysis"]);
    let two = diagnostic(&output, "two");
    assert!(two.contains("failed"), "{two}");
    assert!(two.contains("absent-export-binary.wav"), "{two}");
    assert!(two.contains("cause="), "{two}");
}

#[kithara::test(native, flash(false))]
fn binary_without_an_output_directory_is_a_usage_error() {
    let path = assets::sine_wav_a440_2s().path().expect("fixture");
    let output = invoke(&["--format", "binary", path.to_str().expect("path")], "");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--out"));
}

#[kithara::test(native, flash(false))]
fn an_unknown_format_names_both_formats() {
    let path = assets::sine_wav_a440_2s().path().expect("fixture");
    let output = invoke(&["--format", "xml", path.to_str().expect("path")], "");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("json"), "{stderr}");
    assert!(stderr.contains("binary"), "{stderr}");
}

/// Runs five tracks with `options`, holds the third at its source and
/// interrupts the tool while it waits.
#[cfg(unix)]
async fn interrupted_at_the_third_of_five(options: &[&str]) -> Output {
    let (entered, mut requests) = mpsc::channel(1);
    let (gate, blocked) = watch::channel(false);
    let router = Router::new().route(
        "/third.wav",
        get(move || {
            let entered = entered.clone();
            let mut blocked = blocked.clone();
            async move {
                entered.send(()).await.expect("observer");
                if !*blocked.borrow() {
                    blocked.changed().await.expect("gate");
                }
                assets::sine_wav_a440_2s().bytes().to_vec()
            }
        }),
    );
    let server = TestHttpServer::new(router).await;
    let path = assets::sine_wav_a440_2s().path().expect("fixture");
    let input = records(&[
        json!({"id":"first","src":path}),
        json!({"id":"second","src":path}),
        json!({"id":"third","src":server.url("/third.wav")}),
        json!({"id":"fourth","src":path}),
        json!({"id":"fifth","src":path}),
    ]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_kithara-analyze"))
        .args(options)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("input");
    let opened = timeout(Duration::from_secs(10), requests.recv()).await;
    let signal = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("SIGINT");
    let mut wait = task::spawn_blocking(move || child.wait_with_output());
    let finished = timeout(Duration::from_secs(10), &mut wait).await;
    gate.send(true).expect("release source");
    let output = match finished {
        Ok(output) => output.expect("wait task").expect("output"),
        Err(error) => {
            wait.await.expect("cleanup task").expect("cleanup output");
            panic!("SIGINT must terminate while the source remains blocked: {error:?}");
        }
    };
    assert!(signal.success());
    assert!(opened.expect("third source opens").is_some());
    output
}

#[cfg(unix)]
#[kithara::test(tokio, multi_thread, native, flash(false))]
async fn an_interrupted_binary_batch_leaves_no_file_for_the_cancelled_track() {
    let directory = TestTempDir::new();
    let output = interrupted_at_the_third_of_five(&[
        "--jobs",
        "1",
        "--artifacts",
        "waveform",
        "--format",
        "binary",
        "--out",
        directory.path().to_str().expect("path"),
        "-",
    ])
    .await;
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(
        file_names(&directory),
        ["first.analysis", "second.analysis"]
    );
    let third = diagnostic(&output, "third");
    assert!(third.contains("cancelled"), "{third}");
}

#[cfg(unix)]
#[kithara::test(tokio, multi_thread, native, flash(false))]
async fn sigint_writes_the_inflight_cancellation_and_exits_one() {
    let output =
        interrupted_at_the_third_of_five(&["--jobs", "1", "--artifacts", "waveform", "-"]).await;
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records: Vec<Value> = String::from_utf8(output.stdout)
        .expect("utf8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("record"))
        .collect();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["status"], "complete");
    assert_eq!(records[1]["status"], "complete");
    assert_eq!(records[2]["id"], "third");
    assert_eq!(records[2]["status"], "cancelled");
}

#[kithara::test(native, flash(false))]
fn a_failed_publication_leaves_no_partial_or_temporary_record() {
    let directory = TestTempDir::new();
    std::fs::create_dir(directory.path().join("one.json")).expect("conflicting directory");
    let path = assets::sine_wav_a440_2s().path().expect("fixture");
    let input = json!({"id":"one","src":path}).to_string();
    let output = invoke(
        &[
            "--jobs",
            "1",
            "--artifacts",
            "waveform",
            "--out",
            directory.path().to_str().expect("path"),
            "-",
        ],
        &input,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let files: Vec<_> = std::fs::read_dir(directory.path())
        .expect("directory")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(files, ["one.json"]);
}
