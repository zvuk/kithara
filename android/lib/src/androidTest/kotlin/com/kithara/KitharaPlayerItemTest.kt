package com.kithara

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.BeforeClass
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class KitharaPlayerItemTest {

    companion object {
        @JvmStatic
        @BeforeClass
        fun setUpClass() {
            val context = ApplicationProvider.getApplicationContext<Context>()
            Kithara.initialize(context)
        }
    }

    @Test
    fun initSetsIdAndUrl() {
        val item = KitharaPlayerItem("https://example.com/song.mp3")

        assertTrue(item.id.isNotEmpty())
        assertEquals("https://example.com/song.mp3", item.url)
    }

    @Test
    fun initialStatusIsUnknown() {
        val item = KitharaPlayerItem("https://example.com/song.mp3")

        assertEquals(ItemStatus.Unknown, item.status)
    }

    @Test
    fun initialDurationIsNull() {
        val item = KitharaPlayerItem("https://example.com/song.mp3")

        assertNull(item.duration)
    }

    @Test
    fun initialBufferedDurationIsZero() {
        val item = KitharaPlayerItem("https://example.com/song.mp3")

        assertEquals(0.0, item.bufferedDuration, 0.0)
    }

    @Test
    fun preferredBitrateDefaultsToZero() {
        val item = KitharaPlayerItem("https://example.com/song.mp3")

        assertEquals(0.0, item.preferredPeakBitrate, 0.0)
        assertEquals(0.0, item.preferredPeakBitrateForExpensiveNetworks, 0.0)
    }

    // TODO: restore when preferredPeakBitrate regains a public setter.
    // @Test
    // fun setPreferredBitrateRoundtrip() {
    //     val item = KitharaPlayerItem("https://example.com/song.mp3")
    //
    //     item.preferredPeakBitrate = 128_000.0
    //
    //     assertEquals(128_000.0, item.preferredPeakBitrate, 0.0)
    // }

    @Test
    fun eachItemGetsUniqueId() {
        val first = KitharaPlayerItem("https://example.com/a.mp3")
        val second = KitharaPlayerItem("https://example.com/b.mp3")

        assertNotEquals(first.id, second.id)
    }
}
