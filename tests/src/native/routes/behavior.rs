use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};

use crate::test_server_state::{Content, FixtureBehavior, TestServerState};

pub(crate) fn router() -> Router<Arc<TestServerState>> {
    Router::new().route("/behavior/{token}", get(dispatch))
}

async fn dispatch(
    State(state): State<Arc<TestServerState>>,
    Path(token): Path<String>,
) -> Response {
    let Some(behavior) = state.get_behavior(&token) else {
        return (StatusCode::NOT_FOUND, "no such behavior").into_response();
    };
    state.bump_behavior(&token);
    serve_content(&behavior)
}

fn serve_content(behavior: &FixtureBehavior) -> Response {
    match &behavior.content {
        Content::HtmlError(body) => html_response(body),
        Content::Status(code) => StatusCode::from_u16(*code)
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            .into_response(),
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
