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
    fun initialLoadedRangesAreEmpty() {
        val item = KitharaPlayerItem("https://example.com/song.mp3")

        assertTrue(item.loadedRanges.isEmpty())
    }

    @Test
    fun audioIdMatchesId() {
        val item = KitharaPlayerItem("https://example.com/song.mp3")

        assertEquals(item.id, item.audioId)
    }

    @Test
    fun uuidIsStableForSameItem() {
        val item = KitharaPlayerItem("https://example.com/song.mp3")
        assertEquals(item.uuid, item.uuid)
    }

    @Test
    fun isLiveStreamFromConfig() {
        val item = KitharaPlayerItem("https://example.com/live.m3u8", isLiveStream = true)
        assertTrue(item.isLiveStream)
    }

    @Test
    fun isPlayableLiveAlwaysTrue() {
        val item = KitharaPlayerItem("https://example.com/live.m3u8", isLiveStream = true)
        assertTrue(item.isPlayable(progress = 0.0, ranges = emptyList()))
    }

    @Test
    fun isPlayableWithRanges() {
        val item = KitharaPlayerItem("https://example.com/song.mp3")
        val ranges = listOf(ItemLoadedRange(start = 0.0, duration = 30.0))
        assertTrue(item.isPlayable(progress = 0.0, ranges = ranges))
        assertTrue(item.isPlayable(progress = 15.0, ranges = ranges))
        assertEquals(false, item.isPlayable(progress = 30.0, ranges = ranges))
        assertEquals(false, item.isPlayable(progress = 45.0, ranges = ranges))
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
