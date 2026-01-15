#![forbid(unsafe_code)]

// Internal modules (exposed for advanced usage and testing)
pub mod abr;
pub mod error;
pub mod events;
pub mod fetch;
pub mod keys;
pub mod options;
pub mod playlist;
pub mod session;
pub mod source;
pub mod stream;

// ============================================================================
// Primary public API
// ============================================================================

// ============================================================================
// Advanced types (for ABR customization, monitoring, etc.)
// ============================================================================
pub use abr::{AbrDecision, AbrReason, ThroughputSample, Variant};
pub use error::{HlsError, HlsResult};
pub use events::HlsEvent;
pub use options::{
    AbrOptions, CacheOptions, HlsOptions, KeyContext, KeyOptions, KeyProcessor, NetworkOptions,
    VariantSelector,
};
pub use session::{HlsSession, HlsSource};
pub use source::Hls;
