use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};

use crate::{
    routes::range::build_range_response,
    test_server_state::{Content, Delivery, FixtureBehavior, TestServerState},
};

pub(crate) fn router() -> Router<Arc<TestServerState>> {
    Router::new().route("/behavior/{token}", get(dispatch))
}

async fn dispatch(
    State(state): State<Arc<TestServerState>>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(behavior) = state.get_behavior(&token) else {
        return (StatusCode::NOT_FOUND, "no such behavior").into_response();
    };
    state.bump_behavior(&token);
    serve_content(&behavior, &headers)
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
        } => match behavior.delivery {
            Delivery::Range => build_range_response(bytes, headers, true, true, *content_type),
            Delivery::Normal => {
                build_range_response(bytes, &HeaderMap::new(), true, true, *content_type)
            }
        },
    }
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
        assert_eq!(full.status(), reqwest::StatusCode::OK);
        assert_eq!(full.headers()["content-type"], "audio/mpeg");
        assert_eq!(full.bytes().await.unwrap().as_ref(), b"0123456789");

        let part = client
            .get(server.url(&format!("/behavior/{token}")))
            .header("Range", "bytes=2-5")
            .send()
            .await
            .unwrap();
        assert_eq!(part.status(), reqwest::StatusCode::PARTIAL_CONTENT);
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
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "text/html");
        assert!(resp.text().await.unwrap().contains("captive portal"));

        assert_eq!(state.behavior_hits(&token), Some(1));
    }
}
