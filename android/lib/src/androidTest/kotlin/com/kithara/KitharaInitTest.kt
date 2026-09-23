package com.kithara

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.kithara.okhttp.OkHttpTransport
import okhttp3.OkHttpClient
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotSame
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class KitharaInitTest {
    @Test
    fun multiplePlayersCanBeCreated() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        Kithara.initialize(context, TestTransport.okHttp)

        val p1 = KitharaPlayer()
        val p2 = KitharaPlayer()
        assertNotSame(p1, p2)
    }

    @Test
    fun initializePublishesOneDefaultStore() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        Kithara.initialize(context, TestTransport.okHttp)
        val store = Kithara.defaultStore

        Kithara.initialize(context, TestTransport.okHttp)

        assertSame(store, Kithara.defaultStore)
        assertSame(store, KitharaPlayer.Config().store)
    }

    @Test
    fun initializeRefusesAnotherTransport() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        Kithara.initialize(context, TestTransport.okHttp)
        val store = Kithara.defaultStore

        assertThrows(IllegalStateException::class.java) {
            Kithara.initialize(context, OkHttpTransport(OkHttpClient()))
        }

        assertSame(store, Kithara.defaultStore)
    }

    @Test
    fun nativeRegistryAndStoreCanCreatePlayer() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        Kithara.initialize(context, TestTransport.okHttp)
        val layouts = AssetLayoutRegistry().apply {
            register(FixedLayout, AssetLayoutTarget.File)
        }
        val store = AssetStore(
            root = context.cacheDir.resolve("kithara-layout-test").absolutePath,
            layouts = layouts,
        )

        val player = KitharaPlayer(KitharaPlayer.Config(store = store))

        assertEquals(PlayerStatus.Unknown, player.status)
    }

    @Test
    fun queryIdentityLayoutRegistersForFileAndHls() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        Kithara.initialize(context, TestTransport.okHttp)
        val layout = AssetLayouts.queryIdentity(
            listOf(
                CacheIdentityRule(
                    domains = listOf("media.example.com"),
                    queryParameters = listOf("track_id", "variant"),
                ),
            ),
        )
        val first = layout.root(
            AssetSource.Remote("https://media.example.com/audio.mp3?track_id=one&expires=1"),
        )
        val second = layout.root(
            AssetSource.Remote("https://media.example.com/audio.mp3?track_id=two&expires=1"),
        )
        val refreshed = layout.root(
            AssetSource.Remote("https://media.example.com/audio.mp3?track_id=one&expires=2"),
        )
        assertNotEquals(first, second)
        assertEquals(first, refreshed)

        val layouts = AssetLayoutRegistry().apply {
            register(layout, AssetLayoutTarget.File)
            register(layout, AssetLayoutTarget.Hls)
        }
        val store = AssetStore(
            root = context.cacheDir.resolve("kithara-query-layout-test").absolutePath,
            layouts = layouts,
        )

        val player = KitharaPlayer(KitharaPlayer.Config(store = store))

        assertEquals(PlayerStatus.Unknown, player.status)
    }

    private object FixedLayout : AssetLayout {
        override fun root(source: AssetSource): String = "test-root"

        override fun path(resource: AssetResource): String = "track/track.mp3"
    }
}
