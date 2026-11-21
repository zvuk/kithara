mod command;
mod config;
mod context;
mod inputs;
mod processor;
mod render;
mod slots;
mod tail;
pub mod track;

pub use config::{DeckMixerConfig, DeckMixerConfigPatch, DeckMixerConfigPatchError};
pub use context::{
    install_render_context, invalidate_render_context, publish_render_context, read_render_context,
};
pub use inputs::MixerInputs;
#[cfg(any(test, feature = "mock"))]
pub(crate) use processor::mock as processor_fixture;
pub use processor::{BufferGeometryError, DeckMixer, StreamShape};
pub(crate) use processor::{OfflineProcessor, PreparedDeck};
pub(crate) use render::RenderPass;
pub(crate) use slots::TrackSlots;
