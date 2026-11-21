use std::{
    io::Write,
    process::{Command, Output, Stdio},
};

use axum::{Router, routing::get};
use kithara::platform::{
    time::{Duration, timeout},
    tokio::{
        sync::{mpsc, watch},
        task,
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

#[cfg(unix)]
#[kithara::test(tokio, multi_thread, native, flash(false))]
async fn sigint_writes_the_inflight_cancellation_and_exits_one() {
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
    let input = [
        json!({"id":"first","src":path}),
        json!({"id":"second","src":path}),
        json!({"id":"third","src":server.url("/third.wav")}),
        json!({"id":"fourth","src":path}),
        json!({"id":"fifth","src":path}),
    ]
    .into_iter()
    .map(|record| record.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    let mut child = Command::new(env!("CARGO_BIN_EXE_kithara-analyze"))
        .args(["--jobs", "1", "--artifacts", "waveform", "-"])
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
fn a_failed_rename_leaves_no_partial_or_temporary_record() {
    let directory = TestTempDir::new();
    std::fs::create_dir(directory.path().join("one.json")).expect("conflicting directory");
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
        &input(),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let files: Vec<_> = std::fs::read_dir(directory.path())
        .expect("directory")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(files, ["one.json"]);
}
