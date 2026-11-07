mod config;
mod core;
#[cfg(any(test, feature = "mock"))]
pub(crate) mod fixture;
mod load;
#[cfg(all(test, feature = "mock"))]
pub(crate) mod mock;
#[cfg(all(test, feature = "mock"))]
pub(crate) use node::mock as node_fixture;
mod node;
mod reader;
#[cfg(any(test, feature = "mock"))]
pub(crate) use node::fixture as source_fixture;
#[cfg(any(test, feature = "mock"))]
pub(crate) use reader::mock as packet_mock;
#[cfg(test)]
pub(crate) use reader::tests as packet_tests;
pub(crate) mod scheduler;
mod track;

pub use core::{LoadRefusal, PlayWorker};

pub use config::{PlayWorkerConfig, PlayWorkerConfigPatch};
#[cfg(test)]
pub(crate) use fixture::{terminal_node, terminal_ring};
pub use load::{EngineLoad, EngineLoadSnapshot};
pub use node::DecoderNode;
pub use reader::{PcmPacket, PcmReceiver};
pub use scheduler::StreamWake;
pub use track::{TrackConfig, TrackConfigPatch};

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
