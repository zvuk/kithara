use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, Method, Response, StatusCode, header},
    response::IntoResponse,
    routing::{any, get},
};
use kithara_platform::time::{Duration, sleep};

use crate::{
    fixture_protocol::{HlsRouteKind, HttpErrorRule},
    hls_spec::HlsSpecError,
    hls_stream::GeneratedHls,
    native::routes::range::{build_range_response, build_range_response_with_len},
    test_server_state::TestServerState,
    token_store::is_token,
};

pub(crate) fn router() -> Router<Arc<TestServerState>> {
    Router::new()
        .route("/stream/{hls_spec}", get(master_playlist))
        .route("/stream/{hls_spec}/{media_playlist}", get(media_playlist))
        .route("/stream/{hls_spec}/init/{init_segment}", get(init_segment))
        .route("/stream/{hls_spec}/key.bin", get(key))
        .route("/stream/{hls_spec}/seg/{media_segment}", any(media_segment))
}

async fn master_playlist(
    State(state): State<Arc<TestServerState>>,
    Path(hls_spec): Path<String>,
) -> impl IntoResponse {
    let Some(spec_b64) = hls_spec.strip_suffix(".m3u8") else {
        return (StatusCode::NOT_FOUND, "master playlist must end with .m3u8").into_response();
    };

    match resolve_hls_spec(&state, spec_b64) {
        Ok((fixture, spec_ref)) => {
            if let Some(rule) = fixture.match_http_error(HlsRouteKind::Master, None, None) {
                return inject_error(rule);
            }
            let playlist = fixture.master_playlist(spec_ref);
            response_with_type(StatusCode::OK, "application/vnd.apple.mpegurl", playlist)
        }
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn media_playlist(
    State(state): State<Arc<TestServerState>>,
    Path((hls_spec, media_playlist)): Path<(String, String)>,
) -> impl IntoResponse {
    let Some(variant) = parse_variant_path(&media_playlist, ".m3u8") else {
        return (StatusCode::NOT_FOUND, "invalid media playlist path").into_response();
    };

    match resolve_hls_spec(&state, &hls_spec) {
        Ok((fixture, _)) => {
            if let Some(rule) = fixture.match_http_error(HlsRouteKind::Media, Some(variant), None) {
                return inject_error(rule);
            }
            fixture.media_playlist(variant).map_or_else(
                || StatusCode::NOT_FOUND.into_response(),
                |playlist| {
                    response_with_type(
                        StatusCode::OK,
                        "application/vnd.apple.mpegurl",
                        playlist.to_owned(),
                    )
                },
            )
        }
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn init_segment(
    State(state): State<Arc<TestServerState>>,
    Path((hls_spec, init_segment)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(variant) = parse_variant_path(&init_segment, ".mp4") else {
        return (StatusCode::NOT_FOUND, "invalid init segment path").into_response();
    };

    match resolve_hls_spec(&state, &hls_spec) {
        Ok((fixture, _)) => {
            if let Some(rule) = fixture.match_http_error(HlsRouteKind::Init, Some(variant), None) {
                return inject_error(rule);
            }
            fixture.init_bytes(variant).map_or_else(
                || StatusCode::NOT_FOUND.into_response(),
                |bytes| {
                    build_range_response(&bytes, &headers, true, false, fixture.init_content_type())
                },
            )
        }
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

/// Whether the request is a GET (full body) or HEAD (metadata only).
#[derive(Clone, Copy)]
enum SegmentMethod {
    Get,
    Head,
}

async fn serve_media_segment(
    state: &Arc<TestServerState>,
    hls_spec: &str,
    media_segment: &str,
    headers: &HeaderMap,
    method: SegmentMethod,
) -> axum::response::Response {
    let Some((variant, segment)) = parse_segment_path(media_segment) else {
        return (StatusCode::NOT_FOUND, "invalid media segment path").into_response();
    };

    match resolve_hls_spec(state, hls_spec) {
        Ok((fixture, _)) => {
            if let Some(rule) =
                fixture.match_http_error(HlsRouteKind::Segment, Some(variant), Some(segment))
            {
                return inject_error(rule);
            }
            if matches!(method, SegmentMethod::Get) {
                // Deterministic withhold gate (release-driven, not timer-driven):
                // park the GET body until the test releases it. HEAD stays
                // unblocked so the consumer can still learn the segment size.
                if let Some(gate) = state.segment_gate(hls_spec, variant, segment) {
                    gate.mark_requested();
                    gate.wait_until_released().await;
                }
                let delay_ms = fixture.segment_delay_ms(variant, segment);
                if delay_ms > 0 {
                    sleep(Duration::from_millis(delay_ms)).await;
                }
            }
            fixture.segment_bytes(variant, segment).map_or_else(
                || StatusCode::NOT_FOUND.into_response(),
                |bytes| match method {
                    SegmentMethod::Get => build_range_response(
                        &bytes,
                        headers,
                        true,
                        true,
                        fixture.segment_content_type(),
                    ),
                    SegmentMethod::Head => {
                        let override_len = fixture
                            .segment_len(variant, segment, true)
                            .unwrap_or(bytes.len());
                        build_range_response_with_len(
                            &bytes,
                            headers,
                            false,
                            true,
                            Some(override_len),
                            fixture.segment_content_type(),
                        )
                    }
                },
            )
        }
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

/// Single GET / HEAD entry point for media segments. The handler picks
/// the [`SegmentMethod`] from the request method so the per-method
/// branches stay in [`serve_media_segment`] instead of being duplicated
/// at the route level.
async fn media_segment(
    State(state): State<Arc<TestServerState>>,
    Path((hls_spec, media_segment)): Path<(String, String)>,
    request: Request,
) -> impl IntoResponse {
    let method = match *request.method() {
        Method::GET => SegmentMethod::Get,
        Method::HEAD => SegmentMethod::Head,
        ref other => {
            return (
                StatusCode::METHOD_NOT_ALLOWED,
                format!("media segment route only accepts GET / HEAD, got {other}"),
            )
                .into_response();
        }
    };
    let headers = request.headers().clone();
    serve_media_segment(&state, &hls_spec, &media_segment, &headers, method).await
}

async fn key(
    State(state): State<Arc<TestServerState>>,
    Path(hls_spec): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    match resolve_hls_spec(&state, &hls_spec) {
        Ok((fixture, _)) => {
            if let Some(rule) = fixture.match_http_error(HlsRouteKind::Key, None, None) {
                return inject_error(rule);
            }
            fixture.key_bytes().map_or_else(
                || StatusCode::NOT_FOUND.into_response(),
                |bytes| build_range_response(&bytes, &headers, true, false, None),
            )
        }
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

fn inject_error(rule: &HttpErrorRule) -> Response<Body> {
    let status = StatusCode::from_u16(rule.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = rule
        .body
        .clone()
        .unwrap_or_else(|| format!("injected http error {}", rule.status));
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(body))
        .expect("inject error response")
}

fn response_with_type(
    status: StatusCode,
    content_type: &'static str,
    body: String,
) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .expect("stream response")
}

fn resolve_hls_spec<'a>(
    state: &Arc<TestServerState>,
    spec_ref: &'a str,
) -> Result<(Arc<GeneratedHls>, &'a str), HlsSpecError> {
    if is_token(spec_ref) {
        let Some(fixture) = state.get_hls(spec_ref) else {
            return Err(HlsSpecError::InvalidField {
                field: "token",
                message: "was not found",
            });
        };
        return Ok((fixture, spec_ref));
    }

    state
        .parse_hls_spec(spec_ref)
        .and_then(|spec| state.load_hls(spec).map(|fixture| (fixture, spec_ref)))
}

fn parse_variant_path(path: &str, suffix: &str) -> Option<usize> {
    let variant = path.strip_prefix('v')?.strip_suffix(suffix)?;
    variant.parse().ok()
}

fn parse_segment_path(path: &str) -> Option<(usize, usize)> {
    let path = path.strip_prefix('v')?.strip_suffix(".m4s")?;
    let (variant, segment) = path.split_once('_')?;
    Some((variant.parse().ok()?, segment.parse().ok()?))
}
