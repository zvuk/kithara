package com.kithara

import android.content.Context
import android.util.Log
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.launch
import org.junit.Assert.assertEquals
import org.junit.Assert.fail
import org.junit.BeforeClass
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class SilvercometEnqueueTest {

    companion object {
        private const val TAG = "SilvercometEnqueueTest"

        private const val MP3 = "https://stream.silvercomet.top/track.mp3"
        private const val HLS = "https://stream.silvercomet.top/hls/master.m3u8"
        private const val DRM = "https://stream.silvercomet.top/drm/master.m3u8"

        @JvmStatic
        @BeforeClass
        fun setUpClass() {
            val context = ApplicationProvider.getApplicationContext<Context>()
            Kithara.initialize(context, logLevel = LogLevel.Debug)
        }
    }

    @Test
    fun allSilvercometTracksReachLoaded() {
        runEnqueueScenario(listOf(MP3, HLS, DRM))
    }

    @Test
    fun singleHlsReachesLoaded() {
        runEnqueueScenario(listOf(HLS))
    }

    @Test
    fun singleMp3ReachesLoaded() {
        runEnqueueScenario(listOf(MP3))
    }

    private fun runEnqueueScenario(urls: List<String>) {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val cacheDir = File(context.filesDir, "kithara-cache-test").apply { mkdirs() }

        val player = KitharaPlayer(
            config = KitharaPlayer.Config(cacheDir = cacheDir.absolutePath),
        )

        val terminal = ConcurrentHashMap<String, TrackStatus>()
        val urlById = ConcurrentHashMap<String, String>()
        val latch = CountDownLatch(urls.size)

        val scope = CoroutineScope(Dispatchers.Default)
        val collectJob: Job = scope.launch {
            player.events.collect { event ->
                if (event is KitharaPlayerEvent.TrackStatusChanged) {
                    val url = urlById[event.itemId] ?: "(unknown)"
                    Log.i(TAG, "status: id=${event.itemId} url=$url -> ${event.status}")
                    val isTerminal = event.status is TrackStatus.Loaded ||
                        event.status is TrackStatus.Failed
                    if (isTerminal && terminal.put(event.itemId, event.status) == null) {
                        latch.countDown()
                    }
                }
            }
        }

        try {
            for (url in urls) {
                val item = KitharaPlayerItem(url)
                urlById[item.id] = url
                player.insert(item)
                Log.i(TAG, "enqueued id=${item.id} url=$url")
            }

            val finished = latch.await(45, TimeUnit.SECONDS)
            if (!finished) {
                val missing = urls.filter { url -> terminal.none { urlById[it.key] == url } }
                fail("timeout: missing terminal status for: $missing; seen=${snapshot(terminal, urlById)}")
            }

            val failed = terminal.entries
                .filter { it.value is TrackStatus.Failed }
                .map { (id, st) ->
                    val reason = (st as TrackStatus.Failed).reason
                    "${urlById[id]} -> Failed($reason)"
                }
            assertEquals("tracks must all reach Loaded; got: $failed", emptyList<String>(), failed)
        } finally {
            collectJob.cancel()
            scope.cancel()
            player.removeAllItems()
        }
    }

    private fun snapshot(
        terminal: Map<String, TrackStatus>,
        urlById: Map<String, String>,
    ): String =
        terminal.entries.joinToString(", ") { (id, st) -> "${urlById[id] ?: id}=$st" }
}
