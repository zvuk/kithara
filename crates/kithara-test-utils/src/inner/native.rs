//! Native-only fixtures — один `#[cfg(not(target_arch = "wasm32"))]`
//! гейт на mod-decl этого файла, дальше внутри никаких target-cfg'ов.

pub(crate) mod fmp4;
pub(crate) mod hls_stream;
pub mod http_server;
pub mod routes;
pub(crate) mod test_server_state;

pub use http_server::TestHttpServer;

pub use super::test_server::run_test_server;
