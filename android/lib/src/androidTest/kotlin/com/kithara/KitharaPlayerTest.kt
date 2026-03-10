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
            Kithara.initialize(context)
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
    fun defaultRateIsOne() {
        val player = KitharaPlayer()

        assertEquals(1.0f, player.defaultRate, 0.0f)
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
}
