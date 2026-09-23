package com.kithara

import android.content.Context
import android.util.Log
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import java.io.File
import java.net.URL
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.onSubscription
import kotlinx.coroutines.launch
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.BeforeClass
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class NetworkPolicyTest {

    companion object {
        private const val TAG = "NetworkPolicyTest"
        private const val STATUS_TIMEOUT_SECONDS = 2L
        private const val ADVANCE_TIMEOUT_MS = 2_000L
        private const val ADVANCE_POLL_MS = 250L
        private const val SUBSCRIBE_TIMEOUT_SECONDS = 2L
        private const val PERMITTED_HOST = "127.0.0.1"
        private const val REFUSED_HOST = "localhost"
        private const val POLICY_REFUSAL = "not permitted by network security policy"

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
    fun cleartextReachesOnlyTheHostThePolicyPermits() {
        val permitted = HermeticFixtures.mp3()
        val address = URL(permitted)
        assertEquals("the fixture is served at $PERMITTED_HOST", PERMITTED_HOST, address.host)
        val refused = URL(address.protocol, REFUSED_HOST, address.port, address.file).toString()

        val played = withPlayer("permitted") { player, status ->
            player.insert(KitharaPlayerItem(permitted))
            val loaded = status.await()
            if (loaded is TrackStatus.Loaded) {
                player.play()
                awaitAdvance(player)
            } else {
                "$permitted ended $loaded"
            }
        }
        assertNull("$permitted plays", played)

        val refusal = withPlayer("refused") { player, status ->
            player.insert(KitharaPlayerItem(refused))
            status.await()
        }
        assertTrue(
            "$refused is refused by the network policy, got $refusal",
            refusal is TrackStatus.Failed && refusal.reason.contains(POLICY_REFUSAL),
        )
    }

    private class TerminalStatus {
        val latch = CountDownLatch(1)
        val status = AtomicReference<TrackStatus?>(null)

        fun offer(value: TrackStatus) {
            if (value is TrackStatus.Loaded || value is TrackStatus.Failed) {
                if (status.compareAndSet(null, value)) latch.countDown()
            }
        }

        fun await(): TrackStatus? {
            latch.await(STATUS_TIMEOUT_SECONDS, TimeUnit.SECONDS)
            return status.get()
        }
    }

    private fun <T> withPlayer(name: String, body: (KitharaPlayer, TerminalStatus) -> T): T {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val cacheDir = File(context.filesDir, "kithara-cache-policy-$name").apply {
            deleteRecursively()
            mkdirs()
        }
        val player = KitharaPlayer(
            config = KitharaPlayer.Config(store = AssetStore(root = cacheDir.absolutePath)),
        )
        val status = TerminalStatus()
        val subscribed = CountDownLatch(1)
        val scope = CoroutineScope(Dispatchers.Default)
        scope.launch {
            player.events.onSubscription { subscribed.countDown() }.collect { event ->
                if (event is KitharaPlayerEvent.TrackStatusChanged) {
                    Log.i(TAG, "$name: ${event.itemId} -> ${event.status}")
                    status.offer(event.status)
                }
            }
        }
        try {
            assertTrue(
                "the collector is registered before insert()",
                subscribed.await(SUBSCRIBE_TIMEOUT_SECONDS, TimeUnit.SECONDS),
            )
            return body(player, status)
        } finally {
            player.pause()
            scope.cancel()
            player.removeAllItems()
        }
    }

    private fun awaitAdvance(player: KitharaPlayer): String? {
        val start = player.currentTime
        val deadline = System.currentTimeMillis() + ADVANCE_TIMEOUT_MS
        while (System.currentTimeMillis() < deadline) {
            if (player.currentTime > start) return null
            Thread.sleep(ADVANCE_POLL_MS)
        }
        return "the position stayed at ${player.currentTime}s"
    }
}
