#![forbid(unsafe_code)]

pub mod abr;
pub mod driver;
pub mod error;
pub mod events;
pub mod fetch;
pub mod keys;
pub mod options;
pub mod pipeline;
pub mod playlist;
pub mod session;
pub mod source;

// Public API re-exports
pub use abr::{
    AbrConfig, AbrController, AbrDecision, AbrReason, ThroughputSample, ThroughputSampleSource,
    Variant,
};
pub use driver::DriverError;
pub use error::{HlsError, HlsResult};
pub use events::{EventEmitter, HlsEvent};
pub use fetch::{FetchManager, SegmentStream};
pub use keys::{KeyError, KeyManager};
pub use options::{HlsOptions, KeyContext};
pub use pipeline::{
    BaseStream, PipelineError, PipelineEvent, PipelineResult, PipelineStream, SegmentMeta,
    SegmentPayload,
};
pub use playlist::{PlaylistError, PlaylistManager};
pub use session::{HlsSession, HlsSessionSource};
pub use source::{HlsSource, HlsSourceContract};
