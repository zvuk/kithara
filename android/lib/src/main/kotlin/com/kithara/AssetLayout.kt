package com.kithara

import com.kithara.ffi.FfiAssetLayout
import com.kithara.ffi.FfiAssetLayoutRegistry
import com.kithara.ffi.FfiAssetLayoutTarget
import com.kithara.ffi.FfiAssetResource
import com.kithara.ffi.FfiAssetSource
import com.kithara.ffi.FfiAssetStore

/** Asset identity passed to [AssetLayout.root]. */
sealed interface AssetSource {
    /** A remote asset identified by its URL and optional explicit discriminator. */
    data class Remote(
        val url: String,
        val discriminator: String? = null,
    ) : AssetSource

    /** A local asset identified by its lexical absolute path. */
    data class Local(val path: String) : AssetSource
}

/**
 * Resource identity passed to [AssetLayout.path].
 *
 * [Source] is direct-file media, [Url] covers HLS playlists, init segments,
 * media segments, and keys, and [Named] covers analysis and other derived
 * artifacts.
 */
sealed interface AssetResource {
    /** Direct-file media bytes with their resolved extension. */
    data class Source(val extension: String) : AssetResource

    /** A URL resource such as an HLS playlist, segment, init segment, or key. */
    data class Url(val url: String) : AssetResource

    /** A named derived artifact such as track analysis. */
    data class Named(
        val namespace: String,
        val name: String,
    ) : AssetResource
}

/**
 * Maps asset sources and resources to paths inside Kithara's cache.
 *
 * Implementations must be pure, deterministic, fast, non-blocking,
 * non-throwing, and safe for concurrent calls from background threads. Paths
 * must not contain query text, credentials, or other secrets. [root] is called
 * once when a store scope is created and [path] once when a resource key is
 * minted; cache I/O using that key does not call the layout again.
 *
 * An [AssetResource.Url] contains the full URL. Custom layouts must preserve
 * required query identity without writing raw query text. Kithara's default
 * layout uses a bounded query fingerprint and ignores fragments.
 *
 * [root] must return exactly one non-empty component other than `_index`.
 * [path] must return a non-empty relative path separated by `/`. Components
 * must be ASCII, at most 96 bytes, cannot be `.` or `..`, cannot use Windows
 * device names, cannot contain control bytes or `< > : " / \ | ? *`, and
 * cannot end in a dot, space, or `.tmp`. Reserved-name checks are
 * case-insensitive. Invalid output fails scope or key creation; it is not
 * rewritten and does not fall back to the default layout.
 */
interface AssetLayout {
    /** Returns the single cache-root component for [source]. */
    fun root(source: AssetSource): String

    /** Returns the relative path for [resource] below its asset root. */
    fun path(resource: AssetResource): String
}

/** Playback protocol whose default asset layout can be replaced. */
enum class AssetLayoutTarget {
    File,
    Hls,
}

/**
 * Rust-owned registry of protocol-specific asset layouts.
 *
 * A target has at most one layout. Registering another layout for the same
 * target replaces it immediately in the native registry. An [AssetStore]
 * captures a registry snapshot when the store is created.
 */
class AssetLayoutRegistry internal constructor(
    internal val inner: FfiAssetLayoutRegistry,
) {
    /** Creates an empty registry that uses Kithara's default layout. */
    constructor() : this(FfiAssetLayoutRegistry())

    /** Registers [layout] for [target], replacing an existing registration. */
    fun register(layout: AssetLayout, target: AssetLayoutTarget) {
        inner.register(target.toFfi(), AssetLayoutBridge(layout))
    }
}

/** Shareable Rust-owned asset store used by one or more players. */
class AssetStore internal constructor(
    internal val inner: FfiAssetStore,
) {
    /**
     * Creates an asset store rooted at [root] with a snapshot of [layouts].
     * A `null` root selects Kithara's platform default.
     */
    constructor(
        root: String? = null,
        layouts: AssetLayoutRegistry = AssetLayoutRegistry(),
    ) : this(FfiAssetStore(root, layouts.inner))
}

private class AssetLayoutBridge(
    private val layout: AssetLayout,
) : FfiAssetLayout {
    override fun root(source: FfiAssetSource): String = layout.root(source.toAssetSource())

    override fun path(resource: FfiAssetResource): String = layout.path(resource.toAssetResource())
}

private fun AssetLayoutTarget.toFfi(): FfiAssetLayoutTarget = when (this) {
    AssetLayoutTarget.File -> FfiAssetLayoutTarget.FILE
    AssetLayoutTarget.Hls -> FfiAssetLayoutTarget.HLS
}

private fun FfiAssetSource.toAssetSource(): AssetSource = when (this) {
    is FfiAssetSource.Remote -> AssetSource.Remote(url, discriminator)
    is FfiAssetSource.Local -> AssetSource.Local(path)
}

private fun FfiAssetResource.toAssetResource(): AssetResource = when (this) {
    is FfiAssetResource.Source -> AssetResource.Source(extension)
    is FfiAssetResource.Url -> AssetResource.Url(url)
    is FfiAssetResource.Named -> AssetResource.Named(namespace, name)
}
