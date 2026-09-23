package com.kithara

import android.content.Context
import android.util.Log
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.onSubscription
import kotlinx.coroutines.launch
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.BeforeClass
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The path from a network fixture to the device's own player: one MP3 loads,
 * plays, and the position it reports moves.
 */
@RunWith(AndroidJUnit4::class)
class HermeticPlaybackTest {

    companion object {
        private const val TAG = "HermeticPlaybackTest"
        private const val LOADED_TIMEOUT_SECONDS = 20L
        private const val ADVANCE_TIMEOUT_MS = 15_000L
        private const val ADVANCE_POLL_MS = 250L
        private const val SUBSCRIBE_TIMEOUT_SECONDS = 2L

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
    fun positionAdvancesOnALocalMp3() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val cacheDir = File(context.filesDir, "kithara-cache-playback").apply {
            deleteRecursively()
            mkdirs()
        }
        val player = KitharaPlayer(
            config = KitharaPlayer.Config(store = AssetStore(root = cacheDir.absolutePath)),
        )

        val loaded = CountDownLatch(1)
        val failure = AtomicReference<String?>(null)
        val subscribed = CountDownLatch(1)
        val scope = CoroutineScope(Dispatchers.Default)
        val collectJob: Job = scope.launch {
            player.events.onSubscription { subscribed.countDown() }.collect { event ->
                if (event is KitharaPlayerEvent.TrackStatusChanged) {
                    Log.i(TAG, "status: ${event.itemId} -> ${event.status}")
                    when (val status = event.status) {
                        is TrackStatus.Loaded -> loaded.countDown()
                        is TrackStatus.Failed -> {
                            failure.compareAndSet(null, "load failed: ${status.reason}")
                            loaded.countDown()
                        }
                        else -> Unit
                    }
                }
            }
        }

        try {
            assertTrue(
                "the collector must be registered on the event flow before insert(); " +
                    "a status published while loading is otherwise dropped",
                subscribed.await(SUBSCRIBE_TIMEOUT_SECONDS, TimeUnit.SECONDS),
            )

            player.insert(KitharaPlayerItem(HermeticFixtures.mp3()))
            assertTrue(
                "track must reach a terminal status within ${LOADED_TIMEOUT_SECONDS}s",
                loaded.await(LOADED_TIMEOUT_SECONDS, TimeUnit.SECONDS),
            )
            assertNull(failure.get(), failure.get())

            player.play()
            val advanced = awaitAdvance(player)
            assertNull("player reported ${player.error}", player.error)
            assertNull(failure.get(), failure.get())
            assertTrue(
                "position must move after play(); it stayed at ${player.currentTime}s",
                advanced,
            )
        } finally {
            player.pause()
            collectJob.cancel()
            scope.cancel()
            player.removeAllItems()
        }
    }

    private fun awaitAdvance(player: KitharaPlayer): Boolean {
        val start = player.currentTime
        val deadline = System.currentTimeMillis() + ADVANCE_TIMEOUT_MS
        while (System.currentTimeMillis() < deadline) {
            val now = player.currentTime
            if (now > start) {
                Log.i(TAG, "position advanced ${start}s -> ${now}s")
                return true
            }
            Thread.sleep(ADVANCE_POLL_MS)
        }
        return false
    }
}
