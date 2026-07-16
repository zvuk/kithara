package com.kithara

import com.kithara.ffi.FfiAssetLayout
import com.kithara.ffi.FfiAssetResource
import com.kithara.ffi.FfiAssetSource
import com.kithara.ffi.FfiCacheLayoutRegistration
import com.kithara.ffi.FfiCacheLayoutTarget

/** Asset identity passed to [CacheLayout.root]. */
sealed interface CacheAssetSource {
    /** A remote asset identified by its URL and optional explicit discriminator. */
    data class Remote(
        val url: String,
        val discriminator: String? = null,
    ) : CacheAssetSource

    /** A local asset identified by its lexical absolute path. */
    data class Local(val path: String) : CacheAssetSource
}

/**
 * Resource identity passed to [CacheLayout.path].
 *
 * [Source] is direct-file media, [Url] covers HLS playlists, init segments,
 * media segments, and keys, and [Named] covers analysis and other derived
 * artifacts.
 */
sealed interface CacheAssetResource {
    /** Direct-file media bytes with their resolved extension. */
    data class Source(val extension: String) : CacheAssetResource

    /** A URL resource such as an HLS playlist, segment, init segment, or key. */
    data class Url(val url: String) : CacheAssetResource

    /** A named derived artifact such as track analysis. */
    data class Named(
        val namespace: String,
        val name: String,
    ) : CacheAssetResource
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
 * A [CacheAssetResource.Url] contains the full URL. Custom layouts must
 * preserve required query identity without writing raw query text. Kithara's
 * default layout uses a bounded query fingerprint and ignores fragments.
 *
 * [root] must return exactly one non-empty component other than `_index`.
 * [path] must return a non-empty relative path separated by `/`. Components
 * must be ASCII, at most 96 bytes, cannot be `.` or `..`, cannot use Windows
 * device names, cannot contain control bytes or `< > : " / \ | ? *`, and
 * cannot end in a dot, space, or `.tmp`. Reserved-name checks are
 * case-insensitive. Invalid output fails scope or key creation; it is not
 * rewritten and does not fall back to the default layout.
 */
interface CacheLayout {
    /** Returns the single cache-root component for [source]. */
    fun root(source: CacheAssetSource): String

    /** Returns the relative path for [resource] below its asset root. */
    fun path(resource: CacheAssetResource): String
}

/** Playback protocol whose default cache layout can be replaced. */
enum class CacheLayoutTarget {
    File,
    Hls,
}

/**
 * Cache layouts registered before player creation.
 *
 * A target has at most one layout. Registering another layout for the same
 * target replaces it. A player captures the registry when it is created;
 * subsequent registrations do not affect that player.
 */
class CacheLayoutRegistry {
    private val layouts = mutableMapOf<CacheLayoutTarget, CacheLayout>()

    /** Registers [layout] for [target], replacing an existing registration. */
    fun register(layout: CacheLayout, target: CacheLayoutTarget) {
        layouts[target] = layout
    }

    internal fun toFfiRegistrations(): List<FfiCacheLayoutRegistration> =
        CacheLayoutTarget.entries.mapNotNull { target ->
            layouts[target]?.let { layout ->
                FfiCacheLayoutRegistration(
                    target = target.toFfi(),
                    layout = CacheLayoutBridge(layout),
                )
            }
        }
}

private class CacheLayoutBridge(
    private val layout: CacheLayout,
) : FfiAssetLayout {
    override fun root(source: FfiAssetSource): String = layout.root(source.toCacheSource())

    override fun path(resource: FfiAssetResource): String = layout.path(resource.toCacheResource())
}

private fun CacheLayoutTarget.toFfi(): FfiCacheLayoutTarget = when (this) {
    CacheLayoutTarget.File -> FfiCacheLayoutTarget.FILE
    CacheLayoutTarget.Hls -> FfiCacheLayoutTarget.HLS
}

private fun FfiAssetSource.toCacheSource(): CacheAssetSource = when (this) {
    is FfiAssetSource.Remote -> CacheAssetSource.Remote(url, discriminator)
    is FfiAssetSource.Local -> CacheAssetSource.Local(path)
}

private fun FfiAssetResource.toCacheResource(): CacheAssetResource = when (this) {
    is FfiAssetResource.Source -> CacheAssetResource.Source(extension)
    is FfiAssetResource.Url -> CacheAssetResource.Url(url)
    is FfiAssetResource.Named -> CacheAssetResource.Named(namespace, name)
}
