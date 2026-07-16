package com.kithara

import com.kithara.ffi.FfiAssetResource
import com.kithara.ffi.FfiAssetSource
import com.kithara.ffi.FfiCacheLayoutTarget
import org.junit.Assert.assertEquals
import org.junit.Test

class KitharaPlayerConfigTest {
    @Test
    fun defaultCacheDirFlowsToFfi() {
        val ffi = KitharaPlayer.Config().toFfi(defaultCacheDir = "/global/cache/kithara")

        assertEquals("/global/cache/kithara", ffi.cache.cacheDir)
    }

    @Test
    fun explicitCacheDirOverridesDefault() {
        val ffi = KitharaPlayer.Config(cacheDir = "/player/cache")
            .toFfi(defaultCacheDir = "/global/cache/kithara")

        assertEquals("/player/cache", ffi.cache.cacheDir)
    }

    @Test
    fun layoutsFlowToFfiAndLaterRegistrationWins() {
        val layouts = CacheLayoutRegistry().apply {
            register(FixedLayout("first"), CacheLayoutTarget.File)
            register(FixedLayout("file"), CacheLayoutTarget.File)
            register(FixedLayout("hls"), CacheLayoutTarget.Hls)
        }

        val registrations = KitharaPlayer.Config(layouts = layouts)
            .toFfi(defaultCacheDir = null)
            .cache
            .layouts
        layouts.register(FixedLayout("later"), CacheLayoutTarget.File)

        assertEquals(
            listOf(FfiCacheLayoutTarget.FILE, FfiCacheLayoutTarget.HLS),
            registrations.map { it.target },
        )
        assertEquals(
            "file:remote:https://example.com/master.m3u8:session",
            registrations[0].layout.root(
                FfiAssetSource.Remote(
                    url = "https://example.com/master.m3u8",
                    discriminator = "session",
                )
            ),
        )
        assertEquals(
            "hls:named:analysis:track.analysis",
            registrations[1].layout.path(
                FfiAssetResource.Named(
                    namespace = "analysis",
                    name = "track.analysis",
                )
            ),
        )
    }

    private class FixedLayout(
        private val name: String,
    ) : CacheLayout {
        override fun root(source: CacheAssetSource): String = when (source) {
            is CacheAssetSource.Remote ->
                "$name:remote:${source.url}:${source.discriminator}"

            is CacheAssetSource.Local -> "$name:local:${source.path}"
        }

        override fun path(resource: CacheAssetResource): String = when (resource) {
            is CacheAssetResource.Source -> "$name:source:${resource.extension}"
            is CacheAssetResource.Url -> "$name:url:${resource.url}"
            is CacheAssetResource.Named ->
                "$name:named:${resource.namespace}:${resource.name}"
        }
    }
}
