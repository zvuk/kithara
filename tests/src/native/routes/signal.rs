use std::{convert::Infallible, mem::size_of, sync::Arc};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header, header::HeaderValue},
    response::{IntoResponse, Response},
    routing::get,
};
use futures::stream;
use kithara_encode::{BytesEncodeRequest, BytesEncodeTarget, EncoderFactory};

use crate::{
    native::routes::range::build_range_response,
    signal_pcm::{SignalLength, SignalPcm, signal},
    signal_spec::{
        ResolvedSignalSpec, SignalFormat, SignalKind, SignalRequest, parse_signal_request,
    },
    test_server_state::{EncodedSignal, TestServerState},
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
        .route("/signal/sweep/{spec_with_ext}", get(sweep))
        .route("/signal/silence/{spec_with_ext}", get(silence))
}

async fn sawtooth(
    State(state): State<Arc<TestServerState>>,
    Path(spec_with_ext): Path<String>,
    headers: HeaderMap,
) -> Response {
    handle_signal(&state, SignalKind::Sawtooth, &spec_with_ext, &headers)
}

async fn sawtooth_descending(
    State(state): State<Arc<TestServerState>>,
    Path(spec_with_ext): Path<String>,
    headers: HeaderMap,
) -> Response {
    handle_signal(
        &state,
        SignalKind::SawtoothDescending,
        &spec_with_ext,
        &headers,
    )
}

async fn sine(
    State(state): State<Arc<TestServerState>>,
    Path(spec_with_ext): Path<String>,
    headers: HeaderMap,
) -> Response {
    handle_signal(&state, SignalKind::Sine, &spec_with_ext, &headers)
}

async fn sweep(
    State(state): State<Arc<TestServerState>>,
    Path(spec_with_ext): Path<String>,
    headers: HeaderMap,
) -> Response {
    handle_signal(&state, SignalKind::Sweep, &spec_with_ext, &headers)
}

async fn silence(
    State(state): State<Arc<TestServerState>>,
    Path(spec_with_ext): Path<String>,
    headers: HeaderMap,
) -> Response {
    handle_signal(&state, SignalKind::Silence, &spec_with_ext, &headers)
}

fn handle_signal(
    state: &Arc<TestServerState>,
    kind: SignalKind,
    spec_with_ext: &str,
    headers: &HeaderMap,
) -> Response {
    let request = match resolve_signal_request(state, kind, spec_with_ext) {
        Ok(request) => request,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };

    if matches!(request.format, SignalFormat::Wav)
        && matches!(request.spec.length, SignalLength::Infinite)
    {
        return build_infinite_wav_response(&request.spec);
    }

    let cache_key = encoded_signal_cache_key(kind, spec_with_ext);
    let encoded = if let Some(hit) = state.get_encoded_signal(&cache_key) {
        hit
    } else {
        // Direct-spec URLs (no token) bypass the helper pre-encode
        // path. Encode here on first hit and memoize so later range
        // requests stay cheap. Token-route URLs always pre-encode at
        // fixture build time and never reach this branch.
        let Some(payload) = encode_signal_payload(&request) else {
            return (StatusCode::INTERNAL_SERVER_ERROR, "signal encoding failed").into_response();
        };
        state.insert_encoded_signal(cache_key, payload.clone());
        payload
    };
    build_range_response(
        &encoded.bytes,
        headers,
        true,
        true,
        Some(encoded.content_type),
    )
}

pub(crate) fn encoded_signal_cache_key(kind: SignalKind, spec_with_ext: &str) -> String {
    format!("{}::{spec_with_ext}", kind_str(kind))
}

fn kind_str(kind: SignalKind) -> &'static str {
    match kind {
        SignalKind::Sawtooth => "sawtooth",
        SignalKind::SawtoothDescending => "sawtooth-desc",
        SignalKind::Sine => "sine",
        SignalKind::Sweep => "sweep",
        SignalKind::Silence => "silence",
    }
}

/// Eagerly encode the signal payload for `request`. Test helpers invoke
/// this at fixture build time so that the matching `/signal/...` request
/// handler can serve range requests immediately. Returns `None` for
/// infinite WAV (served via streaming, not via cache).
pub(crate) fn encode_signal_payload(request: &SignalRequest) -> Option<EncodedSignal> {
    match request.format {
        SignalFormat::Wav => encode_wav_payload(&request.spec),
        SignalFormat::Mp3 | SignalFormat::Flac | SignalFormat::Aac | SignalFormat::M4a => {
            encode_compressed_payload(request.format, &request.spec)
        }
    }
}

fn encode_wav_payload(spec: &ResolvedSignalSpec) -> Option<EncodedSignal> {
    if matches!(spec.length, SignalLength::Infinite) {
        return None;
    }
    let bytes = match spec.kind {
        SignalKind::Sawtooth => render_wav(signal::Sawtooth, spec),
        SignalKind::SawtoothDescending => render_wav(signal::SawtoothDescending, spec),
        SignalKind::Sine => render_wav(signal::SineWave(spec.sine_freq_hz?), spec),
        SignalKind::Sweep => render_wav(build_sweep_signal(spec), spec),
        SignalKind::Silence => render_wav(signal::Silence, spec),
    };
    Some(EncodedSignal {
        bytes: Arc::new(bytes),
        content_type: "audio/wav",
    })
}

fn encode_compressed_payload(
    format: SignalFormat,
    spec: &ResolvedSignalSpec,
) -> Option<EncodedSignal> {
    let pcm = SignalPcm::new(
        match spec.kind {
            SignalKind::Sawtooth => SignalEnum::Sawtooth(signal::Sawtooth),
            SignalKind::SawtoothDescending => SignalEnum::SawtoothDesc(signal::SawtoothDescending),
            SignalKind::Sine => SignalEnum::Sine(signal::SineWave(spec.sine_freq_hz?)),
            SignalKind::Sweep => SignalEnum::Sweep(build_sweep_signal(spec)),
            SignalKind::Silence => SignalEnum::Silence(signal::Silence),
        },
        spec.sample_rate,
        spec.channels,
        spec.length,
    );
    let target = encode_target(format);
    let encoder = EncoderFactory::create_bytes(target).ok()?;
    let mut encoded = encoder
        .encode_bytes(BytesEncodeRequest {
            target,
            pcm: &pcm,
            bit_rate: spec.bit_rate,
        })
        .ok()?;
    if format == SignalFormat::Flac
        && let Some(total_frames) = spec.length.total_frames()
    {
        backfill_flac_total_samples(&mut encoded.bytes, total_frames as u64);
    }
    Some(EncodedSignal {
        bytes: Arc::new(encoded.bytes),
        content_type: encoded.content_type,
    })
}

enum SignalEnum {
    Sawtooth(signal::Sawtooth),
    SawtoothDesc(signal::SawtoothDescending),
    Sine(signal::SineWave),
    Sweep(signal::Sweep),
    Silence(signal::Silence),
}

impl signal::SignalFn for SignalEnum {
    fn sample(&self, frame: usize, sample_rate: u32) -> i16 {
        match self {
            Self::Sawtooth(s) => s.sample(frame, sample_rate),
            Self::SawtoothDesc(s) => s.sample(frame, sample_rate),
            Self::Sine(s) => s.sample(frame, sample_rate),
            Self::Sweep(s) => s.sample(frame, sample_rate),
            Self::Silence(s) => s.sample(frame, sample_rate),
        }
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

fn build_infinite_wav_response(spec: &ResolvedSignalSpec) -> Response {
    let body = match spec.kind {
        SignalKind::Sawtooth => stream_wav(signal::Sawtooth, spec),
        SignalKind::SawtoothDescending => stream_wav(signal::SawtoothDescending, spec),
        SignalKind::Sine => match spec.sine_freq_hz {
            Some(freq_hz) => stream_wav(signal::SineWave(freq_hz), spec),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    "sine signal requires a normalized `freq` field",
                )
                    .into_response();
            }
        },
        SignalKind::Sweep => {
            return (StatusCode::BAD_REQUEST, "sweep signal cannot be infinite").into_response();
        }
        SignalKind::Silence => stream_wav(signal::Silence, spec),
    };
    let mut response = body.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
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

fn build_sweep_signal(spec: &ResolvedSignalSpec) -> signal::Sweep {
    let sweep = spec.sweep.expect("validator guarantees sweep params");
    let total_frames = spec
        .length
        .total_frames()
        .expect("validator forbids infinite for sweep");
    signal::Sweep::new(sweep.start_hz, sweep.end_hz, total_frames, sweep.mode)
}

/// `FFmpeg`'s FLAC streaming encoder writes STREAMINFO with `total_samples = 0`.
/// Real FLAC files carry the exact count, and our decoder pipeline reports
/// `duration() = None` without it. Patch the field in place so phase /
/// continuity tests can read fixture duration.
fn backfill_flac_total_samples(bytes: &mut [u8], total_samples: u64) {
    if bytes.len() < 26 || &bytes[0..4] != b"fLaC" {
        return;
    }
    if bytes[4] & 0x7F != 0 {
        return; // first metablock must be STREAMINFO (type 0)
    }
    let body_offset = 8; // 'fLaC' (4) + metablock header (4)
    let field_offset = body_offset + 10; // SR/chan/BPS/total_samples packed at body offset 10
    let cur = u64::from_be_bytes(
        bytes[field_offset..field_offset + 8]
            .try_into()
            .expect("8 bytes"),
    );
    let updated = (cur & 0xFFFF_FFF0_0000_0000_u64) | (total_samples & 0x0000_000F_FFFF_FFFF_u64);
    bytes[field_offset..field_offset + 8].copy_from_slice(&updated.to_be_bytes());
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

fn stream_wav<S: signal::SignalFn>(signal: S, spec: &ResolvedSignalSpec) -> Body {
    let channels = spec.channels;
    let chunk_size = pcm_stream_chunk_size(channels);
    let header = Bytes::from(Vec::<u8>::from(WavHeader::new(
        spec.sample_rate,
        channels,
        None,
    )));
    let state = InfiniteWavState {
        chunk_size,
        header: Some(header),
        pcm: SignalPcm::new(signal, spec.sample_rate, channels, SignalLength::Infinite),
        offset: 0,
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
    chunk_size: usize,
    offset: usize,
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    use super::*;
    use crate::{http_server::TestHttpServer, kithara};

    fn encode(json: &str) -> String {
        URL_SAFE_NO_PAD.encode(json)
    }

    #[kithara::test(tokio)]
    async fn signal_routes_smoke_test() {
        let server = TestHttpServer::new(router().with_state(TestServerState::new())).await;
        let saw = encode(r#"{"seconds":1,"sample_rate":44100,"channels":2}"#);
        let sine = encode(r#"{"seconds":1,"sample_rate":44100,"channels":2,"freq":440}"#);
        let saw_json_ext = encode(r#"{"ext":"wav","seconds":1,"sample_rate":44100,"channels":2}"#);
        let sweep = encode(
            r#"{"seconds":1,"sample_rate":44100,"channels":2,"start_freq":100,"end_freq":8000,"sweep_mode":"linear"}"#,
        );
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
            (format!("/signal/sweep/{sweep}.wav"), 0, b"RIFF".as_slice()),
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
            (format!("/signal/sweep/{sweep}.mp3"), "audio/mpeg"),
            (format!("/signal/sweep/{sweep}.flac"), "audio/flac"),
            (format!("/signal/sweep/{sweep}.aac"), "audio/aac"),
            (format!("/signal/sweep/{sweep}.m4a"), "audio/mp4"),
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

        let invalid_sweep = reqwest::get(server.url(&format!(
            "/signal/sweep/{}.wav",
            encode(r#"{"seconds":1,"sample_rate":44100,"channels":2,"end_freq":8000}"#)
        )))
        .await
        .unwrap();
        assert_eq!(invalid_sweep.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            invalid_sweep.text().await.unwrap(),
            "invalid signal spec field `start_freq`: is required for `sweep`"
        );
    }
}
