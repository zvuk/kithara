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
import kotlinx.coroutines.flow.onSubscription
import kotlinx.coroutines.launch
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.BeforeClass
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Every enqueued track reaches a terminal status against local fixtures: a
 * plain MP3 body, an HLS ladder, and the same ladder behind AES-128.
 */
@RunWith(AndroidJUnit4::class)
class HermeticEnqueueTest {

    companion object {
        private const val TAG = "HermeticEnqueueTest"
        private const val TERMINAL_TIMEOUT_SECONDS = 20L
        private const val SUBSCRIBE_TIMEOUT_SECONDS = 2L

        @JvmStatic
        @BeforeClass
        fun setUpClass() {
            val context = ApplicationProvider.getApplicationContext<Context>()
            Kithara.initialize(context, logLevel = LogLevel.Debug)
        }
    }

    @Before
    fun serverAnswers() {
        TestServerFixture.requireHealthy()
    }

    @Test
    fun aMissingServerUrlFailsSetup() {
        for (value in listOf(null, "", "   ", "not-a-url", "ftp://example.com/a.mp3")) {
            assertThrows(
                "`$value` must be refused rather than reached",
                TestServerFixture.FixtureException::class.java,
            ) { TestServerFixture.parseBaseUrl(value) }
        }
    }

    @Test
    fun everyFixtureKindReachesLoaded() {
        runEnqueueScenario(
            "all",
            listOf(HermeticFixtures.mp3(), HermeticFixtures.hls(), HermeticFixtures.encryptedHls()),
        )
    }

    @Test
    fun singleMp3ReachesLoaded() {
        runEnqueueScenario("mp3", listOf(HermeticFixtures.mp3()))
    }

    @Test
    fun singleHlsReachesLoaded() {
        runEnqueueScenario("hls", listOf(HermeticFixtures.hls()))
    }

    @Test
    fun singleEncryptedHlsReachesLoaded() {
        runEnqueueScenario("aes", listOf(HermeticFixtures.encryptedHls()))
    }

    private fun runEnqueueScenario(scenario: String, urls: List<String>) {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val cacheDir = File(context.filesDir, "kithara-cache-$scenario").apply {
            deleteRecursively()
            mkdirs()
        }
        val store = AssetStore(root = cacheDir.absolutePath)

        val player = KitharaPlayer(
            config = KitharaPlayer.Config(store = store),
        )

        val terminal = ConcurrentHashMap<String, TrackStatus>()
        val latest = ConcurrentHashMap<String, TrackStatus>()
        val urlById = ConcurrentHashMap<String, String>()
        val latch = CountDownLatch(urls.size)

        val subscribed = CountDownLatch(1)
        val scope = CoroutineScope(Dispatchers.Default)
        val collectJob: Job = scope.launch {
            player.events.onSubscription { subscribed.countDown() }.collect { event ->
                if (event is KitharaPlayerEvent.TrackStatusChanged) {
                    val url = urlById[event.itemId] ?: "(unknown)"
                    Log.i(TAG, "status: id=${event.itemId} url=$url -> ${event.status}")
                    latest[event.itemId] = event.status
                    val isTerminal = event.status is TrackStatus.Loaded ||
                        event.status is TrackStatus.Failed
                    if (isTerminal && terminal.put(event.itemId, event.status) == null) {
                        latch.countDown()
                    }
                }
            }
        }

        try {
            assertTrue(
                "the collector must be registered on the event flow before the first insert(); " +
                    "a status published while loading is otherwise dropped",
                subscribed.await(SUBSCRIBE_TIMEOUT_SECONDS, TimeUnit.SECONDS),
            )

            for (url in urls) {
                val item = KitharaPlayerItem(url)
                urlById[item.id] = url
                player.insert(item)
                Log.i(TAG, "enqueued id=${item.id} url=$url")
            }

            val finished = latch.await(TERMINAL_TIMEOUT_SECONDS, TimeUnit.SECONDS)
            if (!finished) {
                val missing = urls.filter { url -> terminal.none { urlById[it.key] == url } }
                fail(
                    "timeout: missing terminal status for: $missing; " +
                        "last seen=${snapshot(latest, urlById)}",
                )
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
        statuses: Map<String, TrackStatus>,
        urlById: Map<String, String>,
    ): String =
        statuses.entries.joinToString(", ") { (id, st) -> "${urlById[id] ?: id}=$st" }
}
