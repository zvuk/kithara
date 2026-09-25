package com.kithara

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.kithara.ffi.FfiFileSourceSettings
import com.kithara.ffi.FfiHlsSourceSettings
import com.kithara.ffi.FfiSourceSettings
import com.kithara.ffi.FfiException
import com.kithara.ffi.FfiSizeProbeMethod
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
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
            Kithara.initialize(context, TestTransport.okHttp)
        }
    }

    @Test
    fun initSetsIdAndUrl() {
        val item = KitharaPlayerItem("https://example.com/song.mp3")

        assertTrue(item.id.isNotEmpty())
        assertEquals("https://example.com/song.mp3", item.url)
    }

    @Test
    fun sourceSettingsReachItemConstructor() {
        val item = KitharaPlayerItem(
            url = "https://example.com/song.mp3",
            audioId = 42u,
            preferredPeakBitrate = 128_000.0,
            sourceSettings = FfiSourceSettings(
                file = FfiFileSourceSettings(readerEventCapacity = 512u),
                hls = null,
            ),
        )
        assertEquals("https://example.com/song.mp3", item.url)
        assertEquals("42", item.audioId)
        assertEquals(128_000.0, item.preferredPeakBitrate, 0.0)
        val hls = KitharaPlayerItem(
            url = "https://example.com/live.m3u8",
            sourceSettings = FfiSourceSettings(
                file = null,
                hls = FfiHlsSourceSettings(
                    sizeProbeMethod = FfiSizeProbeMethod.RANGE_GET,
                    downloadBatchSize = 6u,
                ),
            ),
        )
        assertEquals("https://example.com/live.m3u8", hls.url)
        assertThrows(FfiException.InvalidArgument::class.java) {
            KitharaPlayerItem(
                url = "https://example.com/song.mp3",
                sourceSettings = FfiSourceSettings(
                    file = FfiFileSourceSettings(readerEventCapacity = 4097u),
                    hls = null,
                ),
            )
        }
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

    @Test
    fun preferredBitrateComesFromConstruction() {
        val item = KitharaPlayerItem(
            "https://example.com/song.mp3",
            preferredPeakBitrate = 128_000.0,
            preferredPeakBitrateForExpensiveNetworks = 96_000.0,
        )

        assertEquals(128_000.0, item.preferredPeakBitrate, 0.0)
        assertEquals(96_000.0, item.preferredPeakBitrateForExpensiveNetworks, 0.0)
    }

    @Test
    fun eachItemGetsUniqueId() {
        val first = KitharaPlayerItem("https://example.com/a.mp3")
        val second = KitharaPlayerItem("https://example.com/b.mp3")

        assertNotEquals(first.id, second.id)
    }
}
