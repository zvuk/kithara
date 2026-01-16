//! Source factory trait for kithara.
//!
//! Provides `SourceFactory` trait for creating sources from URLs.

use std::{future::Future, sync::Arc};

use tokio::sync::broadcast;
use url::Url;

use crate::{Source, StreamError};

/// Result of opening a stream source.
pub struct OpenedSource<S, E>
where
    S: Source,
{
    /// The opened source implementing random-access byte reading.
    pub source: Arc<S>,
    /// Broadcast sender for source-specific events.
    pub events_tx: broadcast::Sender<E>,
}

/// Result type for opening a source.
pub type OpenResult<S, E> =
    Result<OpenedSource<S, E>, StreamError<<S as Source>::Error>>;

/// Trait for stream source factories.
///
/// Implementations create sources from URLs and parameters.
/// The associated types define the parameter, event, and source types.
///
/// ## Implementing
///
/// Higher-level crates (kithara-file, kithara-hls) implement this trait
/// for their marker types (`File`, `Hls`).
pub trait SourceFactory: Send + Sync + 'static {
    /// Configuration parameters for opening the source.
    type Params: Send + Default;

    /// Event type emitted by this source.
    type Event: Clone + Send + 'static;

    /// Concrete source type implementing random-access reads.
    type SourceImpl: Source + Send + Sync;

    /// Open a stream from the given URL with the provided parameters.
    ///
    /// Returns an `OpenedSource` containing the source and event channel.
    fn open(
        url: Url,
        params: Self::Params,
    ) -> impl Future<Output = OpenResult<Self::SourceImpl, Self::Event>> + Send;
}
