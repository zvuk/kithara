#![forbid(unsafe_code)]

pub mod abr;
mod api;
mod driver;
pub mod error;
pub mod events;
pub mod fetch;
pub mod keys;
pub mod pipeline;
pub mod playlist;
pub mod session;

// Public API re-exports
pub use abr::{
    AbrConfig, AbrController, AbrDecision, AbrReason, ThroughputSample, ThroughputSampleSource,
    Variant,
};
pub use api::{HlsOptions, HlsSession, HlsSource, HlsSourceContract, KeyContext};
pub use driver::DriverError;
pub use error::{HlsError, HlsResult};
pub use events::{EventEmitter, HlsEvent};
pub use fetch::{FetchManager, SegmentStream};
pub use keys::{KeyError, KeyManager};
pub use pipeline::{
    AbrStream, BaseStream, DrmStream, PipelineCommand, PipelineError, PipelineEvent,
    PipelineResult, PrefetchStream, SegmentMeta, SegmentPayload,
    SegmentStream as PipelineSegmentStream,
};
pub use playlist::{PlaylistError, PlaylistManager};
pub use session::HlsSessionSource;
