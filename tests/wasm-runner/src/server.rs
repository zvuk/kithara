//! Serves the test page and the generated files.
//!
//! The browser keeps its connections alive and opens new ones while old ones
//! stay idle, so every connection is served on its own task: a new connection
//! never waits for an idle one to close.

use std::path::PathBuf;

use axum::{
    Router,
    http::{HeaderName, HeaderValue},
    response::Html,
    routing::get,
};
use kithara_test_utils::TestHttpServer;
use tower_http::{services::ServeDir, set_header::SetResponseHeaderLayer};

/// Serves `index` at `/` and every other path from `dir` on a loopback port
/// until the returned server drops. Cross-origin isolation is on so the tests
/// get `SharedArrayBuffer`.
pub(crate) async fn spawn(dir: PathBuf, index: String) -> TestHttpServer {
    let isolation = [
        ("cross-origin-opener-policy", "same-origin"),
        ("cross-origin-embedder-policy", "require-corp"),
    ];
    let mut router = Router::new()
        .route("/", get(move || async move { Html(index) }))
        .fallback_service(ServeDir::new(dir));
    for (name, value) in isolation {
        router = router.layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        ));
    }
    TestHttpServer::new(router).await
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::TcpStream,
    };

    use super::*;

    async fn connect(server: &TestHttpServer) -> TcpStream {
        let url = server.base_url();
        TcpStream::connect((url.host_str().unwrap(), url.port().unwrap()))
            .await
            .unwrap()
    }

    /// One keep-alive request on `stream`; returns the status line and body.
    async fn fetch(stream: &mut BufReader<TcpStream>, path: &str) -> (String, String) {
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n");
        stream
            .get_mut()
            .write_all(request.as_bytes())
            .await
            .unwrap();
        let mut status = String::new();
        stream.read_line(&mut status).await.unwrap();
        let mut length = 0;
        loop {
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        stream.read_exact(&mut body).await.unwrap();
        (
            status.trim_end().to_owned(),
            String::from_utf8(body).unwrap(),
        )
    }

    /// Chrome holds six idle keep-alive connections per origin; the module a
    /// worker imports next arrives on a seventh and must be answered while the
    /// six stay open; the test's timeout bounds the wait for the seventh.
    #[kithara::test(native, tokio, timeout(Duration::from_secs(1)))]
    async fn answers_a_new_connection_while_six_keep_alive_connections_stay_open() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("module.js"), "export {};").unwrap();
        let server = spawn(dir.path().to_path_buf(), "index".into()).await;

        let mut idle = Vec::new();
        for _ in 0..6 {
            let mut stream = BufReader::new(connect(&server).await);
            assert_eq!(fetch(&mut stream, "/").await.1, "index");
            idle.push(stream);
        }
        let mut seventh = BufReader::new(connect(&server).await);
        let (status, body) = fetch(&mut seventh, "/module.js").await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "export {};");
        for stream in &mut idle {
            assert_eq!(fetch(stream, "/module.js").await.1, "export {};");
        }
    }

    #[kithara::test(native, tokio, timeout(Duration::from_secs(1)))]
    async fn every_response_is_cross_origin_isolated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("worker.js"), "").unwrap();
        let server = spawn(dir.path().to_path_buf(), "index".into()).await;
        for path in ["/", "/worker.js", "/missing.js"] {
            let mut stream = connect(&server).await;
            let request =
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            let response = response.to_ascii_lowercase();
            assert!(
                response.contains("cross-origin-opener-policy: same-origin"),
                "{path}"
            );
            assert!(
                response.contains("cross-origin-embedder-policy: require-corp"),
                "{path}"
            );
        }
    }
}
