package com.kithara

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotSame
import org.junit.Assert.assertSame
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class KitharaInitTest {
    @Test
    fun multiplePlayersCanBeCreated() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        Kithara.initialize(context)

        val p1 = KitharaPlayer()
        val p2 = KitharaPlayer()
        assertNotSame(p1, p2)
    }

    @Test
    fun initializePublishesOneDefaultStore() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        Kithara.initialize(context)
        val store = Kithara.defaultStore

        Kithara.initialize(context)

        assertSame(store, Kithara.defaultStore)
        assertSame(store, KitharaPlayer.Config().store)
    }

    @Test
    fun nativeRegistryAndStoreCanCreatePlayer() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        Kithara.initialize(context)
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

    private object FixedLayout : AssetLayout {
        override fun root(source: AssetSource): String = "test-root"

        override fun path(resource: AssetResource): String = "track/track.mp3"
    }
}
