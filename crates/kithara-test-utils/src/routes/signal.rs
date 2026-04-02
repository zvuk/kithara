//! # Signal generation route.
//! Provides procedural signal generation.
//!
//! ## Routes:
//! - `GET /signal/sawtooth/{spec_with_ext}` — ascending saw-tooth.
//! - `GET /signal/sawtooth-desc/{spec_with_ext}` — descending saw-tooth.
//! - `GET /signal/sine/{spec_with_ext}` — sine wave.
//! - `GET /signal/silence/{spec_with_ext}` — digital silence (all zeros).
//!
//! `spec_with_ext` is a single path segment in the form
//! `<base64url(JSON)>[.<ext>]`, where the optional last `.` separates the
//! encoded JSON payload from the target format extension.
//!
//! ## `spec_with_ext` format
//!
//! `spec_with_ext = <base64url(JSON)>[.<ext>]`
//!
//! ```json
//! {
//!   "ext": "wav",
//!   "sample_rate": 44100,
//!   "channels": 2,
//!   "seconds": 1.0,
//!   "freq": 440.0
//! }
//! ```
//!
//! - `ext`: `wav | mp3 | flac | aac | m4a`; can be passed in the path suffix or in JSON
//! - length: exactly one of `seconds`, `frames`, `file_bytes`, or `infinite`
//! - `freq` is required for `/signal/sine/...` and rejected for sawtooth and silence routes

use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use futures::stream;
use kithara_encode::{BytesEncodeRequest, BytesEncodeTarget, EncodeError, EncoderFactory};

use crate::{
    signal_pcm::{SignalLength, SignalPcm, signal},
    signal_spec::{
        ResolvedSignalSpec, SignalFormat, SignalKind, SignalRequest, parse_signal_request,
    },
    test_server_state::TestServerState,
    token_store::is_token,
    wav::{WavHeader, create_wav_from_signal},
};

const PCM_STREAM_CHUNK_BYTES: usize = 16 * 1024;

pub(crate) fn router() -> Router<Arc<TestServerState>> {
    Router::new()
        .route("/signal/sawtooth/{spec_with_ext}", get(sawtooth))
        .route(
            "/signal/sawtooth-desc/{spec_with_ext}",
            get(sawtooth_descending),
        )
        .route("/signal/sine/{spec_with_ext}", get(sine))
        .route("/signal/silence/{spec_with_ext}", get(silence))
}

async fn sawtooth(
    State(state): State<Arc<TestServerState>>,
    Path(spec_with_ext): Path<String>,
) -> Response {
    handle_signal(&state, SignalKind::Sawtooth, &spec_with_ext)
}

async fn sawtooth_descending(
    State(state): State<Arc<TestServerState>>,
    Path(spec_with_ext): Path<String>,
) -> Response {
    handle_signal(&state, SignalKind::SawtoothDescending, &spec_with_ext)
}

async fn sine(
    State(state): State<Arc<TestServerState>>,
    Path(spec_with_ext): Path<String>,
) -> Response {
    handle_signal(&state, SignalKind::Sine, &spec_with_ext)
}

async fn silence(
    State(state): State<Arc<TestServerState>>,
    Path(spec_with_ext): Path<String>,
) -> Response {
    handle_signal(&state, SignalKind::Silence, &spec_with_ext)
}

fn handle_signal(state: &Arc<TestServerState>, kind: SignalKind, spec_with_ext: &str) -> Response {
    match resolve_signal_request(state, kind, spec_with_ext) {
        Ok(request) => build_signal_response(&request),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

fn resolve_signal_request(
    state: &Arc<TestServerState>,
    kind: SignalKind,
    spec_with_ext: &str,
) -> Result<SignalRequest, crate::signal_spec::SignalRequestError> {
    let (candidate, path_ext) = split_token_candidate(spec_with_ext);
    if is_token(candidate) {
        let Some(request) = state.get_signal(candidate) else {
            return Err(crate::signal_spec::SignalRequestError::InvalidField {
                field: "token",
                message: "was not found",
            });
        };
        if request.spec.kind != kind {
            return Err(crate::signal_spec::SignalRequestError::InvalidField {
                field: "token",
                message: "does not match the requested signal route",
            });
        }
        if let Some(path_ext) = path_ext
            && path_ext != request.format.path_ext()
        {
            return Err(crate::signal_spec::SignalRequestError::InvalidField {
                field: "ext",
                message: "must match the registered token format",
            });
        }
        return Ok(request);
    }

    parse_signal_request(kind, spec_with_ext)
}

fn split_token_candidate(spec_with_ext: &str) -> (&str, Option<&str>) {
    match spec_with_ext.rsplit_once('.') {
        Some((candidate, ext)) if !candidate.is_empty() && !ext.is_empty() => {
            (candidate, Some(ext))
        }
        _ => (spec_with_ext, None),
    }
}

fn build_signal_response(request: &SignalRequest) -> Response {
    match request.format {
        SignalFormat::Wav => build_wav_response(&request.spec),
        SignalFormat::Mp3 | SignalFormat::Flac | SignalFormat::Aac | SignalFormat::M4a => {
            build_encoded_response(request.format, &request.spec)
        }
    }
}

fn build_wav_response(spec: &ResolvedSignalSpec) -> Response {
    match spec.kind {
        SignalKind::Sawtooth => build_wav_response_for_signal(signal::Sawtooth, spec),
        SignalKind::SawtoothDescending => {
            build_wav_response_for_signal(signal::SawtoothDescending, spec)
        }
        SignalKind::Sine => spec.sine_freq_hz.map_or_else(
            || {
                (
                    StatusCode::BAD_REQUEST,
                    "sine signal requires a normalized `freq` field",
                )
                    .into_response()
            },
            |freq_hz| build_wav_response_for_signal(signal::SineWave(freq_hz), spec),
        ),
        SignalKind::Silence => build_wav_response_for_signal(signal::Silence, spec),
    }
}

fn build_wav_response_for_signal<S: signal::SignalFn>(
    signal: S,
    spec: &ResolvedSignalSpec,
) -> Response {
    let mut response = match spec.length {
        SignalLength::Finite { .. } => render_wav(signal, spec).into_response(),
        SignalLength::Infinite => stream_wav(signal, spec).into_response(),
    };
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("audio/wav"),
    );
    response
}

fn render_wav<S: signal::SignalFn>(signal: S, spec: &ResolvedSignalSpec) -> Vec<u8> {
    create_wav_from_signal(SignalPcm::new(
        signal,
        spec.sample_rate,
        spec.channels,
        spec.length,
    ))
}

fn build_encoded_response(format: SignalFormat, spec: &ResolvedSignalSpec) -> Response {
    match spec.kind {
        SignalKind::Sawtooth => build_encoded_response_for_signal(signal::Sawtooth, spec, format),
        SignalKind::SawtoothDescending => {
            build_encoded_response_for_signal(signal::SawtoothDescending, spec, format)
        }
        SignalKind::Sine => spec.sine_freq_hz.map_or_else(
            || {
                (
                    StatusCode::BAD_REQUEST,
                    "sine signal requires a normalized `freq` field",
                )
                    .into_response()
            },
            |freq_hz| build_encoded_response_for_signal(signal::SineWave(freq_hz), spec, format),
        ),
        SignalKind::Silence => build_encoded_response_for_signal(signal::Silence, spec, format),
    }
}

fn build_encoded_response_for_signal<S: signal::SignalFn + Sync>(
    signal: S,
    spec: &ResolvedSignalSpec,
    format: SignalFormat,
) -> Response {
    let pcm = SignalPcm::new(signal, spec.sample_rate, spec.channels, spec.length);
    let target = encode_target(format);
    let encoder = match EncoderFactory::create_bytes(target) {
        Ok(encoder) => encoder,
        Err(error) if is_bad_request(&error) => {
            return (StatusCode::BAD_REQUEST, error.to_string()).into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("signal encoding failed: {error}"),
            )
                .into_response();
        }
    };
    let request = BytesEncodeRequest {
        pcm: &pcm,
        target,
        bit_rate: None,
    };

    match encoder.encode_bytes(request) {
        Ok(encoded) => {
            let mut response = encoded.bytes.into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static(encoded.content_type),
            );
            response
        }

        Err(error) if is_bad_request(&error) => {
            (StatusCode::BAD_REQUEST, error.to_string()).into_response()
        }

        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("signal encoding failed: {error}"),
        )
            .into_response(),
    }
}

fn encode_target(format: SignalFormat) -> BytesEncodeTarget {
    match format {
        SignalFormat::Wav => unreachable!("WAV uses the dedicated route path"),
        SignalFormat::Mp3 => BytesEncodeTarget::Mp3,
        SignalFormat::Flac => BytesEncodeTarget::Flac,
        SignalFormat::Aac => BytesEncodeTarget::Aac,
        SignalFormat::M4a => BytesEncodeTarget::M4a,
    }
}

fn is_bad_request(error: &EncodeError) -> bool {
    matches!(
        error,
        EncodeError::InvalidInput(_) | EncodeError::InvalidMediaInfo(_)
    )
}

fn stream_wav<S: signal::SignalFn>(signal: S, spec: &ResolvedSignalSpec) -> Body {
    let channels = spec.channels;
    let chunk_size = pcm_stream_chunk_size(channels);
    let header = Bytes::from(Vec::<u8>::from(WavHeader::new(
        spec.sample_rate,
        channels,
        None,
    )));
    let state = InfiniteWavState {
        header: Some(header),
        pcm: SignalPcm::new(signal, spec.sample_rate, channels, SignalLength::Infinite),
        offset: 0,
        chunk_size,
    };
    let stream = stream::unfold(state, |mut state| async move {
        if let Some(header) = state.header.take() {
            return Some((Ok::<Bytes, Infallible>(header), state));
        }

        let mut chunk = vec![0u8; state.chunk_size];
        let written = state.pcm.read_pcm_at(state.offset, &mut chunk);
        if written == 0 {
            return None;
        }

        state.offset += written;
        chunk.truncate(written);
        Some((Ok(Bytes::from(chunk)), state))
    });

    Body::from_stream(stream)
}

fn pcm_stream_chunk_size(channels: u16) -> usize {
    let bytes_per_frame = channels as usize * size_of::<i16>();
    PCM_STREAM_CHUNK_BYTES - (PCM_STREAM_CHUNK_BYTES % bytes_per_frame)
}

struct InfiniteWavState<S: signal::SignalFn> {
    header: Option<Bytes>,
    pcm: SignalPcm<S>,
    offset: usize,
    chunk_size: usize,
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    use super::*;
    use crate::http_server::TestHttpServer;

    fn encode(json: &str) -> String {
        URL_SAFE_NO_PAD.encode(json)
    }

    #[tokio::test]
    async fn signal_routes_smoke_test() {
        let server = TestHttpServer::new(router().with_state(TestServerState::new())).await;
        let saw = encode(r#"{"seconds":1,"sample_rate":44100,"channels":2}"#);
        let sine = encode(r#"{"seconds":1,"sample_rate":44100,"channels":2,"freq":440}"#);
        let saw_json_ext = encode(r#"{"ext":"wav","seconds":1,"sample_rate":44100,"channels":2}"#);
        let silence = encode(r#"{"seconds":1,"sample_rate":44100,"channels":2}"#);
        let valid_cases = [
            (
                format!("/signal/sawtooth/{saw}.wav"),
                -32768,
                b"RIFF".as_slice(),
            ),
            (
                format!("/signal/sawtooth-desc/{saw}.wav"),
                32767,
                b"RIFF".as_slice(),
            ),
            (format!("/signal/sine/{sine}.wav"), 0, b"RIFF".as_slice()),
            (
                format!("/signal/sawtooth/{saw_json_ext}"),
                -32768,
                b"RIFF".as_slice(),
            ),
            (
                format!("/signal/silence/{silence}.wav"),
                0,
                b"RIFF".as_slice(),
            ),
        ];

        for (path, first_sample, magic) in valid_cases {
            let response = reqwest::get(server.url(&path)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/wav");

            let bytes = response.bytes().await.unwrap();
            assert!(bytes.len() >= 46);
            assert_eq!(&bytes[0..4], magic);
            assert_eq!(i16::from_le_bytes([bytes[44], bytes[45]]), first_sample);
        }

        let encoded_cases = [
            (format!("/signal/sawtooth/{saw}.mp3"), "audio/mpeg"),
            (format!("/signal/sawtooth/{saw}.flac"), "audio/flac"),
            (format!("/signal/sawtooth/{saw}.aac"), "audio/aac"),
            (format!("/signal/sawtooth/{saw}.m4a"), "audio/mp4"),
        ];

        for (path, content_type) in encoded_cases {
            let response = reqwest::get(server.url(&path)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);

            let bytes = response.bytes().await.unwrap();
            assert!(!bytes.is_empty());
        }

        let invalid = reqwest::get(server.url("/signal/sine/not-base64.wav"))
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            invalid.text().await.unwrap(),
            "signal spec is not valid base64url"
        );

        let unsupported_infinite = reqwest::get(server.url(&format!(
            "/signal/sawtooth/{}.mp3",
            encode(r#"{"infinite":true,"sample_rate":44100,"channels":2}"#)
        )))
        .await
        .unwrap();
        assert_eq!(unsupported_infinite.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            unsupported_infinite.text().await.unwrap(),
            "invalid signal spec field `infinite`: is currently only supported for `wav`"
        );
    }
}
