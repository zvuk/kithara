//! Prefetch worker primitives - re-exports from kithara-worker.
//!
//! This module provides backward compatibility aliases for the generic worker
//! patterns now provided by the `kithara-worker` crate.
//!
//! ## Migration guide
//!
//! Old names (still supported):
//! - `PrefetchSource` → Use `AsyncWorkerSource` from `kithara-worker`
//! - `PrefetchWorker` → Use `AsyncWorker` from `kithara-worker`
//! - `PrefetchedItem<C>` → Use `EpochItem<C>` from `kithara-worker`
//! - `PrefetchConsumer` → Use `EpochConsumer` or `EpochValidator` from `kithara-worker`
//! - `PrefetchResult` → Use `WorkerResult` from `kithara-worker`
//!
//! For blocking/sync sources:
//! - Use `SyncWorkerSource` and `SyncWorker` from `kithara-worker`

#![forbid(unsafe_code)]

// Re-export everything from kithara-worker
pub use kithara_worker::{
    AlwaysValid, AsyncWorker, AsyncWorkerSource, EpochConsumer, EpochItem, EpochValidator,
    ItemValidator, SimpleItem, SyncWorker, SyncWorkerSource, WorkerItem, WorkerResult,
};

// Backward compatibility trait aliases via blanket impls
pub trait PrefetchSource: AsyncWorkerSource {}
impl<T: AsyncWorkerSource> PrefetchSource for T {}

pub trait BlockingSource: SyncWorkerSource {}
impl<T: SyncWorkerSource> BlockingSource for T {}

// Backward compatibility type aliases
pub type PrefetchWorker<S> = AsyncWorker<S>;
pub type PrefetchedItem<C> = EpochItem<C>;
pub type PrefetchConsumer = EpochConsumer;
pub type PrefetchResult = WorkerResult;
pub type BlockingWorker<S> = SyncWorker<S>;

// No tests needed - this module only provides backward compatibility re-exports.
// Core functionality is tested in kithara-worker crate.
