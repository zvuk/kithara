//! Cross-platform test server for file download tests.

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use axum::Router;
    use kithara_test_utils::TestHttpServer;
    use url::Url;

    #[allow(dead_code)]
    pub(crate) struct FileTestServer {
        http: TestHttpServer,
    }

    #[allow(dead_code)]
    impl FileTestServer {
        pub(crate) async fn new(app: Router) -> Self {
            Self {
                http: TestHttpServer::new(app).await,
            }
        }

        pub(crate) fn url(&self, path: &str) -> Url {
            self.http.url(path)
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(unused_imports)]
pub(crate) use native::FileTestServer;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use kithara_test_utils::{fixture_client, fixture_protocol::FileSessionConfig};
    use url::Url;

    #[allow(dead_code)]
    pub(crate) struct FileTestServer {
        session_id: String,
        base_url: Url,
    }

    #[allow(dead_code)]
    impl FileTestServer {
        pub(crate) async fn new(config: FileSessionConfig) -> Self {
            let resp = fixture_client::create_file_session(&config).await;
            Self {
                session_id: resp.session_id,
                base_url: resp.base_url.parse().unwrap(),
            }
        }

        pub(crate) fn url(&self, path: &str) -> Url {
            // Files served at /s/{id}/file/{filename}
            let file_path = format!("file{}", path);
            self.base_url.join(&file_path).unwrap()
        }
    }

    impl Drop for FileTestServer {
        fn drop(&mut self) {
            let id = self.session_id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                fixture_client::delete_session(&id).await;
            });
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[allow(unused_imports)]
pub(crate) use wasm::FileTestServer;
