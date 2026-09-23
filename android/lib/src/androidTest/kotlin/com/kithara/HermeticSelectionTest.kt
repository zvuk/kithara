package com.kithara

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.onSubscription
import kotlinx.coroutines.launch
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.BeforeClass
import org.junit.Test
import org.junit.runner.RunWith

/** Selecting by identity moves the current item; an index outside the queue is refused. */
@RunWith(AndroidJUnit4::class)
class HermeticSelectionTest {

    companion object {
        private const val LOADED_TIMEOUT_SECONDS = 20L
        private const val SUBSCRIBE_TIMEOUT_SECONDS = 2L
        private const val CURRENT_TIMEOUT_MS = 2_000L
        private const val CURRENT_POLL_MS = 50L

        @JvmStatic
        @BeforeClass
        fun setUpClass() {
            val context = ApplicationProvider.getApplicationContext<Context>()
            Kithara.initialize(context, TestTransport.okHttp, logLevel = LogLevel.Debug)
        }
    }

    @Before
    fun serverAnswers() {
        TestServerFixture.requireHealthy()
    }

    @Test
    fun selectionFollowsIdentityAndRefusesAnIndexOutsideTheQueue() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val cacheDir = File(context.filesDir, "kithara-cache-selection").apply {
            deleteRecursively()
            mkdirs()
        }
        val player = KitharaPlayer(
            config = KitharaPlayer.Config(store = AssetStore(root = cacheDir.absolutePath)),
        )
        val first = KitharaPlayerItem(HermeticFixtures.mp3())
        val second = KitharaPlayerItem(HermeticFixtures.hls())

        val loaded = ConcurrentHashMap.newKeySet<String>()
        val bothLoaded = CountDownLatch(2)
        val subscribed = CountDownLatch(1)
        val scope = CoroutineScope(Dispatchers.Default)
        scope.launch {
            player.events.onSubscription { subscribed.countDown() }.collect { event ->
                if (event is KitharaPlayerEvent.TrackStatusChanged &&
                    event.status is TrackStatus.Loaded &&
                    loaded.add(event.itemId)
                ) {
                    bothLoaded.countDown()
                }
            }
        }

        try {
            assertTrue(subscribed.await(SUBSCRIBE_TIMEOUT_SECONDS, TimeUnit.SECONDS))
            player.insert(first)
            player.insert(second, after = first)
            assertTrue(
                "both tracks must load within ${LOADED_TIMEOUT_SECONDS}s; loaded: $loaded",
                bothLoaded.await(LOADED_TIMEOUT_SECONDS, TimeUnit.SECONDS),
            )

            player.selectItem(second)
            assertEquals(second.id, awaitCurrent(player, second.id))

            assertThrows(KitharaError.InvalidArgument::class.java) {
                player.selectItem(at = 99)
            }
        } finally {
            scope.cancel()
            player.removeAllItems()
        }
    }

    private fun awaitCurrent(player: KitharaPlayer, id: String): String? {
        val deadline = System.currentTimeMillis() + CURRENT_TIMEOUT_MS
        while (System.currentTimeMillis() < deadline) {
            val current = player.currentAudioItem?.id
            if (current == id) return current
            Thread.sleep(CURRENT_POLL_MS)
        }
        return player.currentAudioItem?.id
    }
}
