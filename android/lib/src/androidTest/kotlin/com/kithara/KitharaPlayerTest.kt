package com.kithara

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.BeforeClass
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class KitharaPlayerTest {

    companion object {
        @JvmStatic
        @BeforeClass
        fun setUpClass() {
            val context = ApplicationProvider.getApplicationContext<Context>()
            Kithara.initialize(context, TestTransport.okHttp)
        }
    }

    @Test
    fun initCreatesPlayerWithUnknownStatus() {
        val player = KitharaPlayer()

        assertEquals(PlayerStatus.Unknown, player.status)
        assertEquals(0.0, player.currentTime, 0.0)
        assertNull(player.duration)
        assertNull(player.error)
    }

    @Test
    fun playingRateIsOne() {
        val player = KitharaPlayer()

        assertEquals(1.0f, player.playingRate, 0.0f)
    }

    @Test
    fun crossfadeSettingsRoundTripAndRejectInvalidValues() {
        val player = KitharaPlayer()

        val settings = CrossfadeSettings(2.5f, CrossfadeCurve.Linear, 0.25f, 0.3f)
        player.crossfadeSettings = settings
        assertEquals(settings, player.crossfadeSettings)
        org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
            CrossfadeSettings(duration = -1f)
        }
        org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
            CrossfadeSettings(depth = Float.NaN)
        }
    }

    @Test
    fun queuePolicyRoundTrips() {
        val player = KitharaPlayer()
        player.playbackOrder = PlaybackOrder.Shuffle
        player.actionAtItemEnd = ActionAtItemEnd.Pause
        assertEquals(PlaybackOrder.Shuffle, player.playbackOrder)
        assertEquals(ActionAtItemEnd.Pause, player.actionAtItemEnd)
    }

    @Test
    fun configuredCrossfadeSettingsApplyAtConstruction() {
        val settings = CrossfadeSettings(duration = 3.5f)
        val player = KitharaPlayer(KitharaPlayer.Config(crossfadeSettings = settings))

        assertEquals(settings, player.crossfadeSettings)
    }

    @Test
    fun itemsStartsEmpty() {
        val player = KitharaPlayer()

        assertTrue(player.items.isEmpty())
    }

    @Test
    fun removeAllItemsOnEmptyQueueDoesNotCrash() {
        val player = KitharaPlayer()

        player.removeAllItems()

        assertTrue(player.items.isEmpty())
    }

    @Test
    fun removeUpdatesItemsSnapshot() {
        val player = KitharaPlayer()
        val item = KitharaPlayerItem("https://example.com/audio.mp3")

        player.insert(item)
        assertEquals(listOf(item.id), player.items.map(KitharaPlayerItem::id))

        player.remove(item)

        assertTrue(player.items.isEmpty())
    }
}
