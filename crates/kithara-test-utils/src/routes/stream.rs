//! Synthetic HLS generation route family.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, Response, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use kithara_platform::time::{Duration, sleep};

use crate::{
    hls_spec::HlsSpecError, hls_stream::GeneratedHls, test_server_state::TestServerState,
    token_store::is_token,
};

pub(crate) fn router() -> Router<Arc<TestServerState>> {
    Router::new()
        .route("/stream/{hls_spec}", get(master_playlist))
        .route("/stream/{hls_spec}/{media_playlist}", get(media_playlist))
        .route("/stream/{hls_spec}/init/{init_segment}", get(init_segment))
        .route("/stream/{hls_spec}/key.bin", get(key))
        .route(
            "/stream/{hls_spec}/seg/{media_segment}",
            get(media_segment).head(media_segment_head),
        )
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
        Ok((fixture, _)) => fixture.media_playlist(variant).map_or_else(
            || StatusCode::NOT_FOUND.into_response(),
            |playlist| {
                response_with_type(
                    StatusCode::OK,
                    "application/vnd.apple.mpegurl",
                    playlist.to_owned(),
                )
            },
        ),
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
        Ok((fixture, _)) => fixture.init_bytes(variant).map_or_else(
            || StatusCode::NOT_FOUND.into_response(),
            |bytes| {
                build_range_response(&bytes, &headers, true, false, fixture.init_content_type())
            },
        ),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn media_segment(
    State(state): State<Arc<TestServerState>>,
    Path((hls_spec, media_segment)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some((variant, segment)) = parse_segment_path(&media_segment) else {
        return (StatusCode::NOT_FOUND, "invalid media segment path").into_response();
    };

    match resolve_hls_spec(&state, &hls_spec) {
        Ok((fixture, _)) => {
            let delay_ms = fixture.segment_delay_ms(variant, segment);
            if delay_ms > 0 {
                sleep(Duration::from_millis(delay_ms)).await;
            }
            fixture.segment_bytes(variant, segment).map_or_else(
                || StatusCode::NOT_FOUND.into_response(),
                |bytes| {
                    build_range_response(
                        &bytes,
                        &headers,
                        true,
                        true,
                        fixture.segment_content_type(),
                    )
                },
            )
        }
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn media_segment_head(
    State(state): State<Arc<TestServerState>>,
    Path((hls_spec, media_segment)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some((variant, segment)) = parse_segment_path(&media_segment) else {
        return (StatusCode::NOT_FOUND, "invalid media segment path").into_response();
    };

    match resolve_hls_spec(&state, &hls_spec) {
        Ok((fixture, _)) => fixture.segment_bytes(variant, segment).map_or_else(
            || StatusCode::NOT_FOUND.into_response(),
            |bytes| {
                let override_len = fixture
                    .segment_len(variant, segment, true)
                    .unwrap_or(bytes.len());
                build_range_response_with_len(
                    &bytes,
                    &headers,
                    false,
                    true,
                    Some(override_len),
                    fixture.segment_content_type(),
                )
            },
        ),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn key(
    State(state): State<Arc<TestServerState>>,
    Path(hls_spec): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    match resolve_hls_spec(&state, &hls_spec) {
        Ok((fixture, _)) => fixture.key_bytes().map_or_else(
            || StatusCode::NOT_FOUND.into_response(),
            |bytes| build_range_response(&bytes, &headers, true, false, None),
        ),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
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

fn parse_range_header(headers: &HeaderMap) -> Option<(u64, Option<u64>)> {
    let value = headers.get(header::RANGE)?.to_str().ok()?.trim();
    let range = value.strip_prefix("bytes=")?;
    let mut parts = range.splitn(2, '-');
    let start = parts.next()?.trim().parse::<u64>().ok()?;
    let end = match parts.next()?.trim() {
        "" => None,
        value => Some(value.parse::<u64>().ok()?.saturating_add(1)),
    };
    Some((start, end))
}

fn build_range_response(
    data: &[u8],
    headers: &HeaderMap,
    include_body: bool,
    accept_ranges: bool,
    content_type: Option<&'static str>,
) -> Response<Body> {
    build_range_response_with_len(
        data,
        headers,
        include_body,
        accept_ranges,
        None,
        content_type,
    )
}

fn build_range_response_with_len(
    data: &[u8],
    headers: &HeaderMap,
    include_body: bool,
    accept_ranges: bool,
    content_length_override: Option<usize>,
    content_type: Option<&'static str>,
) -> Response<Body> {
    let total = data.len();
    let range = parse_range_header(headers);
    if let Some((start, end_opt)) = range {
        if start >= total as u64 {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                .body(Body::empty())
                .expect("range response");
        }
        let end = end_opt.unwrap_or(total as u64).min(total as u64);
        if start >= end {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                .body(Body::empty())
                .expect("range response");
        }
        let start = start as usize;
        let end = end as usize;
        let content_len = if include_body {
            end - start
        } else {
            content_length_override.unwrap_or(end - start)
        };
        let mut builder = Response::builder()
            .status(if start == 0 && end == total {
                StatusCode::OK
            } else {
                StatusCode::PARTIAL_CONTENT
            })
            .header(header::CONTENT_LENGTH, content_len.to_string());
        if let Some(content_type) = content_type {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }
        if accept_ranges {
            builder = builder.header(header::ACCEPT_RANGES, "bytes");
        }
        if start != 0 || end != total {
            builder = builder.header(
                header::CONTENT_RANGE,
                format!("bytes {}-{}/{}", start, end.saturating_sub(1), total),
            );
        }
        let body = if include_body {
            Body::from(data[start..end].to_vec())
        } else {
            Body::empty()
        };
        return builder.body(body).expect("range response");
    }

    let mut builder = Response::builder().status(StatusCode::OK).header(
        header::CONTENT_LENGTH,
        content_length_override.unwrap_or(total).to_string(),
    );
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    if accept_ranges {
        builder = builder.header(header::ACCEPT_RANGES, "bytes");
    }
    let body = if include_body {
        Body::from(data.to_vec())
    } else {
        Body::empty()
    };
    builder.body(body).expect("stream response")
}
