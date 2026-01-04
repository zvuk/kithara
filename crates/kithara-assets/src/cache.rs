#![forbid(unsafe_code)]

use async_trait::async_trait;
use kithara_storage::{AtomicResource, StreamingResource};
use tokio_util::sync::CancellationToken;

use crate::{error::AssetsResult, key::ResourceKey};

/// Explicit public contract for the assets abstraction.
///
/// ## What this crate is about (normative)
///
/// `kithara-assets` is about *assets* and their *resources*:
/// - an **asset** is a logical unit that may consist of multiple files/resources
///   (e.g. MP3 single file, or HLS playlist + many segments + keys),
/// - a **resource** is addressed by [`ResourceKey`] and can be opened either as:
///   - **atomic** (small files; read/write atomically),
///   - **streaming** (large files; random access, progressive read/write).
///
/// ## What this crate is NOT about (normative)
///
/// This trait does **not** define:
/// - path layout, directories, filename conventions,
/// - string munging / sanitization / hashing,
/// - direct filesystem operations.
///
/// All disk mapping must be encapsulated in the concrete implementation behind this trait.
///
/// ## Leasing / pinning (normative)
///
/// Leasing / pinning is implemented strictly as a decorator (`LeaseAssets`) over a base [`Assets`]
/// implementation. The base `Assets` does not know about pins.
#[async_trait]
pub trait Assets: Send + Sync + 'static {
    /// Open an atomic resource (small object) addressed by `key`.
    ///
    /// This must not perform pinning; pinning is the responsibility of the `LeaseAssets` decorator.
    async fn open_atomic_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> AssetsResult<AtomicResource>;

    /// Open a streaming resource (large object) addressed by `key`.
    ///
    /// This must not perform pinning; pinning is the responsibility of the `LeaseAssets` decorator.
    async fn open_streaming_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> AssetsResult<StreamingResource>;

    /// Open the atomic resource used for persisting the pins index.
    ///
    /// Normative requirements:
    /// - must be a small atomic file-like resource,
    /// - must be excluded from pinning (otherwise the lease decorator will recurse),
    /// - must be stable for the lifetime of this assets instance.
    ///
    /// Key selection (where this lives on disk) is an implementation detail of the concrete
    /// `Assets` implementation. Higher layers must not construct or assume a `ResourceKey` here.
    async fn open_pins_index_resource(
        &self,
        cancel: CancellationToken,
    ) -> AssetsResult<AtomicResource>;

    /// Delete an entire asset (all resources under `asset_root`).
    ///
    /// ## Normative
    /// - This is used by eviction/GC decorators (LRU, quota enforcement).
    /// - Base `Assets` implementations must NOT apply pin/lease semantics here.
    /// - Higher layers must ensure pinned assets are not deleted (decorators enforce this).
    async fn delete_asset(&self, asset_root: &str, cancel: CancellationToken) -> AssetsResult<()>;

    /// Open the atomic resource used for persisting the LRU index.
    ///
    /// ## Normative
    /// - must be a small atomic file-like resource,
    /// - must be stable for the lifetime of this assets instance.
    ///
    /// Key selection (where this lives on disk) is an implementation detail of the concrete
    /// `Assets` implementation.
    async fn open_lru_index_resource(
        &self,
        cancel: CancellationToken,
    ) -> AssetsResult<AtomicResource>;
}
