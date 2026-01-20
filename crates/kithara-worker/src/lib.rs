//! Generic worker primitives for async and blocking data sources.
//!
//! This crate provides reusable worker patterns for processing data in background tasks:
//!
//! - **`AsyncWorker`**: Async worker for async sources (e.g., HTTP streams)
//!   - Uses `AsyncWorkerSource` trait with async `fetch_next()`
//!   - Produces `EpochItem<C>` with epoch tracking for seek invalidation
//!   - Validated by `EpochValidator`
//!
//! - **`SyncWorker`**: Blocking worker for sync sources (e.g., audio decoders)
//!   - Uses `SyncWorkerSource` trait with sync `fetch_next()`
//!   - Produces `SimpleItem<C>` without epoch overhead
//!   - Validated by `AlwaysValid` (no invalidation)
//!
//! Both workers follow the same protocol: commands via mpsc, data via kanal channels,
//! with backpressure and EOF signaling.
//!
//! ## Architecture
//!
//! All workers implement the base `Worker` trait for unified execution:
//!
//! ```ignore
//! use kithara_worker::{Worker, AsyncWorker, SyncWorker};
//!
//! // Async worker
//! let async_worker = AsyncWorker::new(source, cmd_rx, data_tx);
//! async_worker.run().await;
//!
//! // Sync worker
//! let sync_worker = SyncWorker::new(source, cmd_rx, data_tx);
//! sync_worker.run().await;  // Internally uses spawn_blocking
//! ```

#![forbid(unsafe_code)]

// Module declarations
mod async_worker;
mod item;
mod result;
mod sync_worker;
mod traits;
mod validator;

// Public re-exports
pub use async_worker::AsyncWorker;
pub use item::{EpochItem, Fetch, SimpleItem, WorkerItem};
pub use result::WorkerResult;
pub use sync_worker::SyncWorker;
pub use traits::{AsyncWorkerSource, SyncWorkerSource, Worker};
pub use validator::{AlwaysValid, EpochConsumer, EpochValidator, ItemValidator};

// Export mock types when testing or test-utils feature is enabled
// Note: Only Worker trait has auto-generated mock (MockWorker)
// AsyncWorkerSource and SyncWorkerSource require manual mocking due to associated types
#[cfg(any(test, feature = "test-utils"))]
pub use traits::MockWorker;
