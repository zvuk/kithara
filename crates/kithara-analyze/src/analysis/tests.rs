use std::{collections::BTreeMap, io::Cursor};

use axum::{Router, routing::get};
use clap::Parser;
use kithara::{
    analysis::AnalysisOutcome,
    platform::{
        CancelScope,
        time::{Duration, timeout},
        tokio::sync::{mpsc, watch},
    },
};
use kithara_test_fixtures::assets;
use kithara_test_utils::TestHttpServer;

use super::Engine;
use crate::input::{Args, Batch, Track};

#[kithara::test(tokio, native, flash(false))]
async fn full_selection_settles_at_the_decoded_native_rate() {
    let path = assets::rhythm_wav_deck_a_120bpm_48k()
        .path()
        .expect("disk fixture");
    let args =
        Args::try_parse_from(["analyze", path.to_str().expect("fixture path")]).expect("args");
    let batch = Batch::try_new(args, Cursor::new("")).expect("input");
    let scope = CancelScope::new(None);
    let engine = Engine::try_new(
        &batch.args,
        super::jobs(&batch.args).expect("jobs"),
        scope.token(),
    )
    .expect("engine");
    let outcome = engine
        .analyze(&batch.tracks[0])
        .await
        .expect("source opens");
    let AnalysisOutcome::Complete(progress) = outcome else {
        panic!("full source must settle: {outcome:?}")
    };
    let analysis = progress.analysis();
    assert_eq!(analysis.source_sample_rate().get(), 48_000);
    assert_eq!(analysis.extent(), Some(576_000));
    assert!(analysis.waveform().is_some());
    assert!(analysis.beat().is_some());
    assert!(analysis.key().is_some());
    assert!(analysis.fingerprint().waveform().is_some());
    assert!(analysis.fingerprint().beat().is_some());
    assert!(analysis.fingerprint().key().is_some());
}

#[kithara::test(tokio, native, flash(false))]
async fn missing_source_does_not_stop_the_batch() {
    let path = assets::sine_wav_a440_2s().path().expect("fixture");
    let input = [
        serde_json::json!({"id": "first", "src": path}),
        serde_json::json!({"id": "missing", "src": path.with_file_name("absent-export-test.wav")}),
        serde_json::json!({"id": "last", "src": path}),
    ]
    .into_iter()
    .map(|record| record.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    let args = Args::try_parse_from(["analyze", "--artifacts", "waveform,key", "-"]).expect("args");
    let batch = Batch::try_new(args, Cursor::new(input)).expect("input");
    let scope = CancelScope::new(None);
    let (tx, mut rx) = mpsc::channel(3);
    let jobs = super::jobs(&batch.args).expect("jobs");
    let task = kithara::platform::tokio::task::spawn(batch.analyze(jobs, scope.token(), tx));
    let mut results = BTreeMap::new();
    while let Some((track, outcome)) = rx.recv().await {
        results.insert(track.id, outcome);
    }
    assert_eq!(results.len(), 3);
    for id in ["first", "last"] {
        assert!(matches!(
            results.get(id),
            Some(Ok(AnalysisOutcome::Complete(_)))
        ));
    }
    assert!(
        !results
            .get("missing")
            .expect("failure record")
            .as_ref()
            .expect_err("missing source fails")
            .to_string()
            .is_empty()
    );
    task.await.expect("join").expect("batch");
}

#[kithara::test(tokio, multi_thread, native, flash(false))]
async fn four_jobs_admit_four_sources_and_match_one_job() {
    let (entered, mut requests) = mpsc::channel(8);
    let (gate, handler_gate) = watch::channel(false);
    let wav = assets::sine_wav_a440_2s().bytes().to_vec();
    let router = Router::new().route(
        "/{name}",
        get(move || {
            let entered = entered.clone();
            let mut gate = handler_gate.clone();
            let wav = wav.clone();
            async move {
                entered.send(()).await.expect("request observer");
                if !*gate.borrow() {
                    gate.changed().await.expect("gate");
                }
                wav
            }
        }),
    );
    let server = TestHttpServer::new(router).await;
    let input = (0..4)
        .map(|id| {
            serde_json::json!({"id": format!("t{id}"), "src": server.url(&format!("/{id}.wav"))})
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let batch = Batch::try_new(
        Args::try_parse_from(["analyze", "--jobs", "4", "-"]).expect("args"),
        Cursor::new(&input),
    )
    .expect("input");
    let scope = CancelScope::new(None);
    let (tx, mut rx) = mpsc::channel(4);
    let jobs = super::jobs(&batch.args).expect("jobs");
    let task = kithara::platform::tokio::task::spawn(batch.analyze(jobs, scope.token(), tx));
    let admitted = timeout(Duration::from_secs(5), async {
        for _ in 0..4 {
            requests.recv().await.expect("four admitted sources");
        }
    })
    .await;
    gate.send(true).expect("open gate");
    let mut parallel = BTreeMap::new();
    while let Some((track, result)) = rx.recv().await {
        parallel.insert(track.id, settled(result));
    }
    task.await.expect("join").expect("parallel batch");
    let batch = Batch::try_new(
        Args::try_parse_from(["analyze", "--jobs", "1", "-"]).expect("args"),
        Cursor::new(input),
    )
    .expect("input");
    let (tx, mut rx) = mpsc::channel(4);
    let jobs = super::jobs(&batch.args).expect("jobs");
    let task = kithara::platform::tokio::task::spawn(batch.analyze(jobs, scope.token(), tx));
    let mut serial = BTreeMap::new();
    while let Some((track, result)) = rx.recv().await {
        serial.insert(track.id, settled(result));
    }
    task.await.expect("join").expect("serial batch");
    assert!(
        admitted.is_ok(),
        "four jobs must open independently before any finishes"
    );
    assert_eq!(parallel.len(), 4);
    assert_eq!(parallel, serial);
}

fn settled(result: Result<AnalysisOutcome, super::OpenError>) -> serde_json::Value {
    let outcome = result.expect("source opens");
    let AnalysisOutcome::Complete(progress) = outcome else {
        panic!("source settles: {outcome:?}")
    };
    let analysis = progress.analysis();
    serde_json::json!({
        "rate": analysis.source_sample_rate().get(),
        "origin": analysis.media_origin().map(|origin| origin.as_secs_f64()),
        "extent": analysis.extent(),
        "coverage": analysis.coverage().iter().map(|range| [range.start, range.end]).collect::<Vec<_>>(),
        "fingerprint": [analysis.fingerprint().waveform(), analysis.fingerprint().beat(), analysis.fingerprint().key()],
        "key": analysis.key().map(|estimate| (estimate.key(), estimate.strength())),
        "waveform": analysis.waveform().map(|wave| (wave.span_frames().get(), wave.buckets().iter().map(|bucket| [bucket.low(), bucket.mid(), bucket.high()]).collect::<Vec<_>>())),
        "beat": analysis.beat().map(|beat| (beat.artifact().bpm(), beat.artifact().beats(), beat.artifact().beat_confidence(), beat.artifact().downbeats(), beat.artifact().downbeat_confidence(), beat.confidence())),
    })
}

#[kithara::test(tokio, native, flash(false))]
async fn selection_and_fold_preserve_the_requested_density() {
    let path = assets::rhythm_wav_deck_a_120bpm_48k()
        .path()
        .expect("fixture");
    let source = path.to_str().expect("path");
    let scope = CancelScope::new(None);
    let args = Args::try_parse_from(["analyze", "--artifacts", "beat,key", source]).expect("args");
    let engine =
        Engine::try_new(&args, super::jobs(&args).expect("jobs"), scope.token()).expect("engine");
    let track = Track::try_from(source).expect("track");
    let outcome = engine.analyze(&track).await.expect("source");
    let AnalysisOutcome::Complete(progress) = outcome else {
        panic!("settles: {outcome:?}")
    };
    assert!(progress.analysis().waveform().is_none());
    assert!(progress.analysis().fingerprint().waveform().is_none());
    assert!(progress.analysis().beat().is_some());
    assert!(progress.analysis().key().is_some());
    let mut waves = Vec::new();
    for fold in [None, Some("10")] {
        let mut options = vec!["analyze", "--artifacts", "waveform", source];
        if let Some(fold) = fold {
            options.extend(["--waveform-fold", fold]);
        }
        let args = Args::try_parse_from(options).expect("args");
        let engine = Engine::try_new(&args, super::jobs(&args).expect("jobs"), scope.token())
            .expect("engine");
        let outcome = engine.analyze(&track).await.expect("source");
        let AnalysisOutcome::Complete(progress) = outcome else {
            panic!("settles: {outcome:?}")
        };
        let analysis = progress.analysis();
        let waveform = analysis.waveform().expect("waveform");
        assert!(analysis.fingerprint().beat().is_none());
        assert!(analysis.fingerprint().key().is_none());
        waves.push((
            waveform.buckets().len(),
            waveform.span_frames().get(),
            analysis
                .fingerprint()
                .waveform()
                .expect("waveform tag")
                .to_owned(),
        ));
    }
    assert_eq!(waves[0].2, "wave:fold1:v2");
    assert_eq!(waves[1].2, "wave:fold10:v2");
    assert_eq!(waves[1].0, waves[0].0.div_ceil(10));
    assert_eq!(waves[1].1, waves[0].1 * 10);
}
