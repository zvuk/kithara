use std::{fs, num::NonZeroU16, path::PathBuf};

use axum::{Router, routing::get};
use kithara_beat::{BeatGridModel, BeatGridState, GridBeat, Meter, RawBeatGrid, SCHEMA_VERSION};
use kithara_download::{Downloader, DownloaderConfig};
use kithara_net::{Headers, HttpClient, NetOptions};
use kithara_platform::CancelToken;
use kithara_test_utils::{TestHttpServer, bufpool::pools as test_pools, kithara};
use kithara_waveform::{Bucket, Waveform};
use url::Url;

use super::{ArtifactFetch, ArtifactLoadError, ArtifactSource, MAX_ARTIFACT_BYTES};
use crate::resource::ResourceSrc;

fn grid() -> RawBeatGrid {
    RawBeatGrid {
        schema_version: SCHEMA_VERSION,
        model_id: "fixture".to_owned(),
        revision: 3,
        state: BeatGridState::Final,
        duration: Some(2.0),
        bpm: 120.0,
        beats: vec![
            GridBeat {
                at: 0.0,
                ordinal: 0,
                confidence: Some(1.0),
            },
            GridBeat {
                at: 0.5,
                ordinal: 1,
                confidence: None,
            },
        ],
        downbeats: Vec::new(),
        meter: Some(Meter {
            beats_per_bar: NonZeroU16::new(4).unwrap_or(NonZeroU16::MIN),
            origin_beat_ordinal: 0,
        }),
    }
}

fn waveform() -> Waveform {
    Waveform::try_from(vec![Bucket::new(0.25, 0.5, 0.75)]).expect("BUG: bands are in range")
}

fn downloader() -> Downloader {
    let client = HttpClient::new(NetOptions::default(), test_pools(), CancelToken::never());
    Downloader::new(DownloaderConfig::for_client(client).build())
}

/// A source the tests point an artifact at, written into the scratch
/// directory the harness owns.
fn file(name: &str, bytes: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("kithara-artifact-{name}"));
    fs::write(&path, bytes).expect("BUG: scratch write");
    path
}

fn audio() -> ResourceSrc {
    ResourceSrc::Url(Url::parse("https://audio.example/track.mp3").expect("BUG: valid URL"))
}

/// Serves `body` at `/artifact` for as long as the returned server lives.
async fn serve(body: Vec<u8>) -> TestHttpServer {
    TestHttpServer::new(Router::new().route("/artifact", get(move || async move { body }))).await
}

#[kithara::test(tokio)]
async fn a_value_publishes_without_touching_io() {
    let audio = audio();
    // No downloader at all: a value that reached for one would fail here.
    let fetch = ArtifactFetch::new(&audio, None, None, None);
    let source = ArtifactSource::Value(kithara_platform::sync::Arc::new(waveform()));

    let loaded = source
        .load(&fetch)
        .await
        .expect("BUG: a value needs no I/O");

    assert_eq!(loaded.buckets().len(), 1);
}

#[kithara::test(tokio)]
async fn a_path_yields_the_document_it_holds() {
    let bytes = serde_json::to_vec(&grid()).expect("BUG: serializable");
    let path = file("grid.json", &bytes);
    let audio = audio();
    let fetch = ArtifactFetch::new(&audio, None, None, None);
    let source = ArtifactSource::<BeatGridModel>::Source(ResourceSrc::Path(path));

    let loaded = source
        .load(&fetch)
        .await
        .expect("BUG: a written grid reads");

    assert_eq!(loaded.as_raw().revision, 3);
}

#[kithara::test(tokio)]
async fn a_url_yields_the_document_it_serves() {
    let mut bytes = Vec::new();
    waveform().write_to(&mut bytes);
    let server = serve(bytes).await;
    let url = server.url("/artifact");
    let audio = audio();
    let downloader = downloader();
    let fetch = ArtifactFetch::new(&audio, Some(&downloader), None, None);
    let source = ArtifactSource::<Waveform>::Source(ResourceSrc::Url(url));

    let loaded = source
        .load(&fetch)
        .await
        .expect("BUG: a served waveform reads");

    assert_eq!(loaded.buckets().len(), 1);
}

/// A document of the wrong kind is a typed error, never a quiet fall back to
/// analysing the track locally.
#[kithara::test(tokio)]
async fn a_document_of_the_wrong_kind_is_an_error() {
    let path = file("wrong.json", b"{\"not\": \"a grid\"}");
    let audio = audio();
    let fetch = ArtifactFetch::new(&audio, None, None, None);
    let source = ArtifactSource::<BeatGridModel>::Source(ResourceSrc::Path(path));

    let error = source.load(&fetch).await.expect_err("BUG: not a grid");

    assert!(
        matches!(error, ArtifactLoadError::Decode { kind, .. } if kind == "beat grid"),
        "{error}"
    );
}

#[kithara::test(tokio)]
async fn a_missing_path_is_an_error() {
    let audio = audio();
    let fetch = ArtifactFetch::new(&audio, None, None, None);
    let source = ArtifactSource::<Waveform>::Source(ResourceSrc::Path(PathBuf::from(
        "/kithara/no/such/waveform.bin",
    )));

    let error = source.load(&fetch).await.expect_err("BUG: nothing to read");

    assert!(matches!(error, ArtifactLoadError::Fetch { .. }), "{error}");
}

#[kithara::test(tokio)]
async fn a_document_past_the_cap_is_refused_unread() {
    let path = file("huge.bin", &vec![0_u8; MAX_ARTIFACT_BYTES + 1]);
    let audio = audio();
    let fetch = ArtifactFetch::new(&audio, None, None, None);
    let source = ArtifactSource::<Waveform>::Source(ResourceSrc::Path(path));

    let error = source.load(&fetch).await.expect_err("BUG: past the cap");

    assert!(
        matches!(error, ArtifactLoadError::TooLarge { limit, .. } if limit == MAX_ARTIFACT_BYTES),
        "{error}"
    );
}

#[kithara::test(tokio)]
async fn a_scheme_no_artifact_is_read_over_is_refused() {
    let audio = audio();
    let downloader = downloader();
    let fetch = ArtifactFetch::new(&audio, Some(&downloader), None, None);
    let url = Url::parse("ftp://example.com/grid.json").expect("BUG: valid URL");
    let source = ArtifactSource::<BeatGridModel>::Source(ResourceSrc::Url(url));

    let error = source
        .load(&fetch)
        .await
        .expect_err("BUG: not a read scheme");

    assert!(matches!(error, ArtifactLoadError::Scheme { .. }), "{error}");
}

/// The audio source's credentials belong to the audio host. Configuring an
/// artifact URL elsewhere must not be a way to send them somewhere else.
#[kithara::test(tokio)]
async fn audio_credentials_do_not_follow_an_artifact_to_another_host() {
    let mut headers = Headers::default();
    headers.insert("Authorization", "Bearer audio-token");
    let mut bytes = Vec::new();
    waveform().write_to(&mut bytes);
    let server = serve(bytes).await;
    let url = server.url("/artifact");
    let audio = audio();
    let downloader = downloader();
    let fetch = ArtifactFetch::new(&audio, Some(&downloader), Some(&headers), None);

    assert!(
        fetch.headers_for(&url).is_none(),
        "a third host gets no audio credentials"
    );
    assert!(
        fetch
            .headers_for(&Url::parse("https://audio.example/grid.json").expect("BUG: valid URL"))
            .is_some(),
        "the audio host keeps its own"
    );
}

#[kithara::test(tokio)]
async fn a_cancelled_load_reports_itself_cancelled() {
    let mut bytes = Vec::new();
    waveform().write_to(&mut bytes);
    let server = serve(bytes).await;
    let url = server.url("/artifact");
    let audio = audio();
    // The load epoch this artifact belonged to is over before it starts: a
    // removed or reloaded resource must not still read its artifact.
    let downloader = downloader();
    let cancel = CancelToken::never().child();
    cancel.cancel();
    let fetch = ArtifactFetch::new(&audio, Some(&downloader), None, Some(&cancel));
    let source = ArtifactSource::<Waveform>::Source(ResourceSrc::Url(url));

    let error = source.load(&fetch).await.expect_err("BUG: cancelled");

    assert!(
        matches!(error, ArtifactLoadError::Cancelled { .. }),
        "{error}"
    );
}
