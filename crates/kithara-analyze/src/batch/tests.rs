use std::io::Cursor;

use axum::{Router, routing::get};
use clap::Parser;
use kithara::platform::{
    CancelScope,
    time::{Duration, timeout},
    tokio::{
        sync::{mpsc, watch},
        task,
    },
};
use kithara_test_fixtures::assets;
use kithara_test_utils::TestHttpServer;
use serde_json::{Value, json};

use crate::{Args, Batch};

#[kithara::test(tokio, multi_thread, native, flash(false))]
async fn cancelling_the_third_track_keeps_two_and_never_starts_the_last_two() {
    let (entered, mut opening) = mpsc::channel(1);
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
    let args = Args::try_parse_from(["analyze", "--jobs", "1", "--artifacts", "waveform", "-"])
        .expect("args");
    let batch = Batch::try_new(args, Cursor::new(input)).expect("input");
    let scope = CancelScope::new(None);
    let run = task::spawn(batch.run(scope.token(), Vec::new()));
    let opened = timeout(Duration::from_secs(5), opening.recv()).await;
    scope.cancel();
    gate.send(true).expect("release source");
    let (success, output) = run.await.expect("join").expect("batch");
    assert!(opened.expect("third source opens").is_some());
    assert!(!success);
    let records: Vec<Value> = String::from_utf8(output)
        .expect("utf8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("record"))
        .collect();
    assert_eq!(records.len(), 3, "queued tracks must have no records");
    assert_eq!(records[0]["id"], "first");
    assert_eq!(records[1]["id"], "second");
    assert_eq!(records[0]["status"], "complete");
    assert_eq!(records[1]["status"], "complete");
    assert_eq!(
        records[2],
        json!({"id":"third","src":server.url("/third.wav"),"status":"cancelled"})
    );
}
