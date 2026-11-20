use std::{
    io::Cursor,
    iter::once,
    num::{NonZeroU32, NonZeroU64},
};

use clap::Parser;
use kithara::{
    analysis::{
        AnalysisError, AnalysisFingerprint, AnalysisOutcome, AnalysisProgress, AnalysisToken,
        BeatArtifact, BeatGridModel, BeatSnapshot, BeatState, Bucket, KeyEstimate, Mode,
        MusicalKey, PitchClass, RangeSet, RawBeatGrid, TrackAnalysis, Waveform,
    },
    platform::{CancelScope, time::Duration},
};
use kithara_test_fixtures::assets;
use serde_json::{Value, json};

use super::json as encode;
use crate::{Args, Batch, analysis::Engine, input::Track};

fn publication() -> AnalysisProgress {
    let beat = BeatSnapshot::new(
        BeatArtifact::new(
            120.0,
            vec![
                (0, Some(0.8)),
                (24_000, None),
                (48_000, Some(0.6)),
                (72_000, Some(0.9)),
            ],
            vec![(0, Some(0.7))],
        ),
        BeatState::Final,
        once(36_000..40_000).collect(),
    );
    let waveform = Waveform::try_new(
        vec![Bucket::new(0.1, 0.4, 0.2), Bucket::new(0.8, 0.2, 0.3)],
        NonZeroU64::new(10_240).expect("span"),
    )
    .expect("waveform");
    let key = KeyEstimate::new(
        MusicalKey {
            tonic: PitchClass::A,
            mode: Mode::Minor,
        },
        0.71,
    )
    .expect("key");
    let analysis = TrackAnalysis::builder()
        .token(AnalysisToken::from("t1"))
        .source_sample_rate(NonZeroU32::new(48_000).expect("rate"))
        .media_origin(Duration::ZERO)
        .extent(96_000)
        .coverage(RangeSet::from_iter(once(0..96_000)))
        .settled(true)
        .revision(0)
        .fingerprint(
            AnalysisFingerprint::builder()
                .waveform("wave:fold10:v2")
                .beat("beat:test")
                .key("key:test")
                .build(),
        )
        .beat(beat)
        .waveform(waveform)
        .key(key)
        .build();
    AnalysisProgress::try_from(analysis).expect("settled analysis")
}

fn track() -> Track {
    Track {
        id: "t1".to_owned(),
        src: "/music/one.flac".to_owned(),
    }
}

#[kithara::test]
fn full_record_preserves_every_artifact_and_confidence() {
    let progress = publication();
    let raw = RawBeatGrid::from(BeatGridModel::try_from(progress.analysis()).expect("grid"));
    let record: Value = serde_json::from_slice(
        &encode(&track(), &Ok(AnalysisOutcome::Complete(progress))).expect("encode"),
    )
    .expect("json");
    assert_eq!(record["id"], "t1");
    assert_eq!(record["status"], "complete");
    assert_eq!(record["missing"], json!([]));
    assert_eq!(record["tool_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        record["source"],
        json!({"sample_rate": 48_000, "media_origin_s": 0.0, "extent_frames": 96_000})
    );
    assert_eq!(record["coverage"], json!([[0, 96_000]]));
    assert_eq!(
        record["fingerprint"],
        json!({"waveform": "wave:fold10:v2", "beat": "beat:test", "key": "key:test"})
    );
    assert_eq!(record["beat"]["state"], "final");
    assert_eq!(record["beat"]["bpm"], 120.0);
    assert_eq!(
        record["beat"]["beats"],
        json!([{"frame":0,"confidence":0.8},{"frame":24_000,"confidence":null},{"frame":48_000,"confidence":0.6},{"frame":72_000,"confidence":0.9}])
    );
    assert_eq!(
        record["beat"]["downbeats"],
        json!([{"frame":0,"confidence":0.7}])
    );
    assert_eq!(record["beat"]["confidence"], json!(0.75));
    assert_eq!(record["beat"]["unanalysed"], json!([[36_000, 40_000]]));
    assert_eq!(
        record["grid"],
        serde_json::from_slice::<Value>(&serde_json::to_vec(&raw).expect("raw grid"))
            .expect("grid json")
    );
    assert!(record["grid_unavailable"].is_null());
    assert_eq!(
        record["key"],
        json!({"tonic":9,"mode":"minor","strength":0.71})
    );
    assert_eq!(
        record["waveform"],
        json!({"bands":["low","mid","high"],"span_frames":10_240,"buckets":[[0.1,0.4,0.2],[0.8,0.2,0.3]]})
    );
}

#[kithara::test]
fn failures_and_cancellations_never_export_the_last_publication() {
    let cases = [
        (
            AnalysisOutcome::Failed {
                error: AnalysisError::Empty,
                last: Some(publication()),
            },
            "failed",
        ),
        (
            AnalysisOutcome::Cancelled {
                last: Some(publication()),
            },
            "cancelled",
        ),
    ];
    for (outcome, status) in cases {
        let record: Value =
            serde_json::from_slice(&encode(&track(), &Ok(outcome)).expect("encode")).expect("json");
        assert_eq!(record["status"], status);
        assert_eq!(
            record.as_object().expect("object").len(),
            if status == "failed" { 4 } else { 3 }
        );
        assert!(record.get("waveform").is_none());
        if status == "failed" {
            assert_eq!(record["error"], AnalysisError::Empty.to_string());
        }
    }
}

#[kithara::test]
fn incomplete_record_keeps_gaps_and_an_unmapped_origin() {
    let analysis = TrackAnalysis::builder()
        .token(AnalysisToken::from("t1"))
        .source_sample_rate(NonZeroU32::new(44_100).expect("rate"))
        .extent(100)
        .coverage(RangeSet::from_iter([0..20, 40..100]))
        .revision(0)
        .settled(true)
        .build();
    let progress = AnalysisProgress::try_from(analysis).expect("settled");
    let record: Value = serde_json::from_slice(
        &encode(&track(), &Ok(AnalysisOutcome::Incomplete(progress))).expect("encode"),
    )
    .expect("json");
    assert_eq!(record["status"], "incomplete");
    assert_eq!(record["missing"], json!([[20, 40]]));
    assert!(record["source"]["media_origin_s"].is_null());
    assert!(record["grid"].is_null());
    assert!(record["grid_unavailable"].as_str().is_some());
    assert!(record["key"].is_null());
    assert!(record["waveform"].is_null());
}

#[kithara::test(tokio, native, flash(false))]
async fn silence_has_a_null_key_and_the_key_fingerprint() {
    let path = assets::signal_wav_silence_1s().path().expect("fixture");
    let args = Args::try_parse_from([
        "analyze",
        "--artifacts",
        "key",
        path.to_str().expect("path"),
    ])
    .expect("args");
    let batch = Batch::try_new(args, Cursor::new("")).expect("input");
    let scope = CancelScope::new(None);
    let engine = Engine::try_new(&batch.args, scope.token()).expect("engine");
    let result = engine.analyze(&batch.tracks[0]).await;
    let record: Value =
        serde_json::from_slice(&encode(&batch.tracks[0], &result).expect("encode")).expect("json");
    assert_eq!(record["status"], "complete");
    assert!(record["key"].is_null());
    assert!(record["fingerprint"]["key"].as_str().is_some());
    assert!(record["fingerprint"].get("waveform").is_none());
    assert!(record["fingerprint"].get("beat").is_none());
}
