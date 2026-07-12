use std::io;

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use bytes::Bytes;
use futures::{StreamExt, stream};
use kithara::platform::{
    sync::Arc,
    time::{Duration, sleep},
};

use crate::{
    routes::range::build_range_response,
    test_server_state::{Content, Delivery, FixtureBehavior, TestServerState},
};

pub(crate) fn router() -> Router<Arc<TestServerState>> {
    Router::new()
        .route("/behavior/{token}", get(dispatch).head(dispatch))
        .route(
            "/behavior/{token}/{*rest}",
            get(dispatch_nested).head(dispatch_nested),
        )
}

async fn dispatch(
    State(state): State<Arc<TestServerState>>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Response {
    serve(&state, &token, &headers)
}

async fn dispatch_nested(
    State(state): State<Arc<TestServerState>>,
    Path((token, _rest)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    serve(&state, &token, &headers)
}

fn serve(state: &TestServerState, token: &str, headers: &HeaderMap) -> Response {
    let Some(behavior) = state.get_behavior(token) else {
        return (StatusCode::NOT_FOUND, "no such behavior").into_response();
    };
    state.bump_behavior(token);
    serve_content(&behavior, headers)
}

fn serve_content(behavior: &FixtureBehavior, headers: &HeaderMap) -> Response {
    match &behavior.content {
        Content::HtmlError(body) => html_response(body),
        Content::Status(code) => StatusCode::from_u16(*code)
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            .into_response(),
        Content::StaticBytes {
            bytes,
            content_type,
        } => match &behavior.delivery {
            Delivery::Range => build_range_response(bytes, headers, true, true, *content_type),
            Delivery::Normal => {
                build_range_response(bytes, &HeaderMap::new(), true, true, *content_type)
            }
            Delivery::EarlyClose { after_bytes } => {
                early_close_response(bytes, headers, *after_bytes, *content_type)
            }
            Delivery::StallAfter { after_bytes } => {
                stall_response(bytes, *after_bytes, *content_type)
            }
            Delivery::Throttle { chunk, delay_ms } => {
                throttle_response(bytes, *chunk, *delay_ms, *content_type)
            }
        },
    }
}

fn stall_response(
    bytes: &[u8],
    after_bytes: usize,
    content_type: Option<&'static str>,
) -> Response {
    // Advertise the full length, deliver only a prefix, then keep the
    // connection open forever — the client must abort on its own inactivity
    // budget, never on a server-side close.
    let total = bytes.len();
    let prefix = Bytes::copy_from_slice(&bytes[..after_bytes.min(total)]);
    let body =
        Body::from_stream(stream::iter(vec![Ok::<_, io::Error>(prefix)]).chain(stream::pending()));
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, total.to_string());
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder.body(body).expect("stall response")
}

fn throttle_response(
    bytes: &[u8],
    chunk: usize,
    delay_ms: u64,
    content_type: Option<&'static str>,
) -> Response {
    // Stream the full body from 0 in `chunk`-sized pieces with `delay_ms`
    // between, but advertise the full length immediately so the decoder can
    // report duration before the body finishes arriving.
    let total = bytes.len();
    let pieces: Vec<Bytes> = bytes
        .chunks(chunk.max(1))
        .map(Bytes::copy_from_slice)
        .collect();
    let body = Body::from_stream(stream::unfold(
        pieces.into_iter(),
        move |mut it| async move {
            let piece = it.next()?;
            sleep(Duration::from_millis(delay_ms)).await;
            Some((Ok::<_, io::Error>(piece), it))
        },
    ));
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, total.to_string());
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder.body(body).expect("throttle response")
}

fn early_close_response(
    bytes: &[u8],
    headers: &HeaderMap,
    after_bytes: usize,
    content_type: Option<&'static str>,
) -> Response {
    // A range request gets a normal partial response — this is the on-demand
    // seek path that must succeed after the sequential stream closes early.
    if headers.contains_key(header::RANGE) {
        return build_range_response(bytes, headers, true, true, content_type);
    }
    // Sequential GET: advertise the full length but deliver only `after_bytes`
    // then close, so the client observes a premature end-of-stream. A stream
    // body keeps hyper from validating the chunk length against the explicit
    // Content-Length; ending short closes the connection mid-message.
    let total = bytes.len();
    let truncated = Bytes::copy_from_slice(&bytes[..after_bytes.min(total)]);
    let body = Body::from_stream(stream::iter(vec![Ok::<_, io::Error>(truncated)]));
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, total.to_string())
        .header(header::ACCEPT_RANGES, "bytes");
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder.body(body).expect("early close response")
}

fn html_response(body: &str) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, HeaderValue::from_static("text/html"))],
        body.to_string(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{http_server::TestHttpServer, kithara, test_server_state::*};

    #[kithara::test(tokio)]
    async fn static_bytes_range_serves_partial_and_counts() {
        let state = TestServerState::new();
        let token = state.insert_behavior(FixtureBehavior {
            content: Content::StaticBytes {
                bytes: Arc::new(b"0123456789".to_vec()),
                content_type: Some("audio/mpeg"),
            },
            delivery: Delivery::Range,
        });
        let server = TestHttpServer::new(router().with_state(Arc::clone(&state))).await;
        let client = reqwest::Client::new();

        let full = client
            .get(server.url(&format!("/behavior/{token}")))
            .send()
            .await
            .unwrap();
        assert_eq!(full.status(), StatusCode::OK);
        assert_eq!(full.headers()["content-type"], "audio/mpeg");
        assert_eq!(full.bytes().await.unwrap().as_ref(), b"0123456789");

        let part = client
            .get(server.url(&format!("/behavior/{token}")))
            .header("Range", "bytes=2-5")
            .send()
            .await
            .unwrap();
        assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(part.bytes().await.unwrap().as_ref(), b"2345");

        assert_eq!(state.behavior_hits(&token), Some(2));
    }

    #[kithara::test(tokio)]
    async fn html_error_behavior_serves_html_and_counts() {
        let state = TestServerState::new();
        let token = state.insert_behavior(FixtureBehavior {
            content: Content::HtmlError("<html>captive portal</html>"),
            delivery: Delivery::Normal,
        });
        let server = TestHttpServer::new(router().with_state(Arc::clone(&state))).await;

        let resp = reqwest::get(server.url(&format!("/behavior/{token}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "text/html");
        assert!(resp.text().await.unwrap().contains("captive portal"));

        assert_eq!(state.behavior_hits(&token), Some(1));
    }

    #[kithara::test(tokio)]
    async fn early_close_advertises_full_length_and_range_still_works() {
        let state = TestServerState::new();
        let token = state.insert_behavior(FixtureBehavior {
            content: Content::StaticBytes {
                bytes: Arc::new(vec![7u8; 1000]),
                content_type: Some("audio/mpeg"),
            },
            delivery: Delivery::EarlyClose { after_bytes: 400 },
        });
        let server = TestHttpServer::new(router().with_state(Arc::clone(&state))).await;
        let client = reqwest::Client::new();

        // Sequential GET advertises the full length (1000) but closes after 400
        // bytes, so draining the body surfaces a premature end-of-stream.
        let resp = client
            .get(server.url(&format!("/behavior/{token}")))
            .send()
            .await;
        let body = match resp {
            Ok(resp) => {
                assert_eq!(resp.status(), StatusCode::OK);
                assert_eq!(resp.headers()[header::CONTENT_LENGTH], "1000");
                resp.bytes().await
            }
            Err(err) => Err(err),
        };
        assert!(
            body.is_err(),
            "early-close body must not complete the advertised length"
        );

        let part = client
            .get(server.url(&format!("/behavior/{token}")))
            .header("Range", "bytes=600-699")
            .send()
            .await
            .unwrap();
        assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(part.bytes().await.unwrap().len(), 100);
    }

    #[kithara::test(tokio)]
    async fn throttle_sends_full_length_and_complete_body() {
        let state = TestServerState::new();
        let token = state.insert_behavior(FixtureBehavior {
            content: Content::StaticBytes {
                bytes: Arc::new(vec![3u8; 50]),
                content_type: Some("audio/mpeg"),
            },
            delivery: Delivery::Throttle {
                chunk: 10,
                delay_ms: 1,
            },
        });
        let server = TestHttpServer::new(router().with_state(Arc::clone(&state))).await;
        let resp = reqwest::get(server.url(&format!("/behavior/{token}")))
            .await
            .unwrap();
        assert_eq!(resp.headers()[header::CONTENT_LENGTH], "50");
        assert_eq!(resp.bytes().await.unwrap().len(), 50);
    }

    #[kithara::test(tokio)]
    async fn nested_child_path_dispatches_to_same_behavior() {
        let state = TestServerState::new();
        let token = state.insert_behavior(FixtureBehavior {
            content: Content::StaticBytes {
                bytes: Arc::new(b"abcdef".to_vec()),
                content_type: Some("audio/mpeg"),
            },
            delivery: Delivery::Range,
        });
        let server = TestHttpServer::new(router().with_state(Arc::clone(&state))).await;
        let resp = reqwest::get(server.url(&format!("/behavior/{token}/x.mp3")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.bytes().await.unwrap().as_ref(), b"abcdef");
        assert_eq!(state.behavior_hits(&token), Some(1));
    }
}
