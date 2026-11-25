use std::io::Cursor;

use clap::{Parser, error::ErrorKind};

use super::{Args, Batch};

#[kithara::test]
fn identifiers_come_from_local_and_url_file_stems() {
    let args = Args::try_parse_from(["analyze", "a/one.mp3", "https://host/b/two.flac?token=x"])
        .expect("arguments");
    let batch = Batch::try_new(args, Cursor::new("")).expect("valid sources");
    assert_eq!(
        batch
            .tracks
            .iter()
            .map(|track| track.id.as_str())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(batch.args.waveform_fold.get(), 1);
}

#[kithara::test]
fn stdin_records_preserve_ids_and_sources() {
    let args = Args::try_parse_from(["analyze", "-"]).expect("arguments");
    let batch = Batch::try_new(args, Cursor::new("{\"id\":\"t1\",\"src\":\"https://host/1.mp3\"}\n{\"id\":\"t2\",\"src\":\"/music/2.flac\"}\n")).expect("valid records");
    assert_eq!(batch.tracks[0].id, "t1");
    assert_eq!(batch.tracks[1].src, "/music/2.flac");
}

#[kithara::test]
fn duplicates_name_both_sources_before_work() {
    let args = Args::try_parse_from(["analyze", "a/one.mp3", "b/one.flac"]).expect("arguments");
    let error = Batch::try_new(args, Cursor::new("")).expect_err("duplicate id");
    assert_eq!(error.kind(), ErrorKind::ValueValidation);
    let message = error.to_string();
    assert!(message.contains("a/one.mp3"));
    assert!(message.contains("b/one.flac"));
}

#[kithara::test]
fn malformed_stdin_names_the_line() {
    for invalid in [
        "not json",
        "{}",
        "{\"id\":3,\"src\":\"x\"}",
        "{\"id\":\"x\",\"src\":false}",
    ] {
        let args = Args::try_parse_from(["analyze", "-"]).expect("arguments");
        let input = format!("{{\"id\":\"ok\",\"src\":\"ok.wav\"}}\n{invalid}\n");
        let error = Batch::try_new(args, Cursor::new(input)).expect_err("malformed line");
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        assert!(error.to_string().contains("line 2"));
    }
}

#[kithara::test]
fn invalid_options_are_usage_errors() {
    for options in [
        ["--artifacts", "spectrogram"],
        ["--format", "binary"],
        ["--waveform-fold", "0"],
        ["--jobs", "0"],
    ] {
        assert!(Args::try_parse_from(["analyze", "one.wav", options[0], options[1]]).is_err());
    }
}

#[kithara::test]
fn stdin_identifiers_are_single_file_names() {
    for id in ["", ".", "..", "../outside", "nested/name", "nested\\name"] {
        let args = Args::try_parse_from(["analyze", "-"]).expect("arguments");
        let line = serde_json::json!({"id": id, "src": "one.wav"}).to_string();
        assert!(Batch::try_new(args, Cursor::new(line)).is_err());
    }
}
