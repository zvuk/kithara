mod hls;
mod hls_oracle;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
mod shared;
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(target_arch = "wasm32")]
pub(crate) use hls::post_token;
pub use hls::{CreateHlsError, CreatedHls, HlsFixtureBuilder};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::router;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{
    BehaviorHandle, InitGateHandle, PrivateTestServer, SegmentGateHandle, TestServerHelper,
    run_test_server,
};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use shared::shared;
#[cfg(target_arch = "wasm32")]
pub use wasm::TestServerHelper;

#[cfg(not(target_arch = "wasm32"))]
pub use crate::test_server_state::{Content, Delivery, FixtureBehavior, NetworkMode};
