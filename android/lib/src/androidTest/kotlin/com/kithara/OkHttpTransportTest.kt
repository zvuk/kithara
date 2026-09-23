package com.kithara

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.kithara.net.HttpCall
import com.kithara.net.HttpCallback
import com.kithara.okhttp.OkHttpTransport
import java.io.File
import java.net.URL
import java.nio.ByteBuffer
import java.util.Base64
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import okhttp3.Cache
import okhttp3.Dispatcher
import okhttp3.Headers
import okhttp3.OkHttpClient
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class OkHttpTransportTest {

    @Before
    fun serverAnswers() {
        TestServerFixture.requireHealthy()
    }

    @Test
    fun aBodyLargerThanOneBufferArrivesInOrder() {
        val url = TestServerFixture.signal(LARGE_SIGNAL)
        val expected = URL(url).readBytes()
        assertTrue(
            "the fixture body of ${expected.size} bytes must exceed one read buffer",
            expected.size > BUFFER_BYTES,
        )

        val recorder = CallRecorder()
        val call = OkHttpTransport(OkHttpClient()).start("GET", url, NO_HEADERS, null, recorder)
        assertEquals(200, recorder.expectResponse().status)

        val chunks = readBody(call, recorder)
        assertTrue("a body that size takes more than one read, not ${chunks.size}", chunks.size > 1)
        assertArrayEquals(expected, join(chunks))
    }

    @Test
    fun noBytesArriveWhileNoReadIsOutstanding() {
        val recorder = CallRecorder()
        val call = OkHttpTransport(OkHttpClient())
            .start("GET", TestServerFixture.signal(LARGE_SIGNAL), NO_HEADERS, null, recorder)
        assertEquals(200, recorder.expectResponse().status)

        val silence = recorder.next(QUIET_MS)
        assertTrue("nothing is reported without a read, got ${describe(silence)}", silence == null)

        call.read(ByteBuffer.allocateDirect(BUFFER_BYTES))
        assertTrue("the read after the quiet window carries bytes", recorder.expectRead() > 0)
        call.cancel()
    }

    @Test
    fun aCancelledCallEndsOnceAndLeavesItsBufferAlone() {
        val recorder = CallRecorder()
        val call = OkHttpTransport(OkHttpClient())
            .start("GET", TestServerFixture.createBehavior(pacedBody()), NO_HEADERS, null, recorder)
        assertEquals(200, recorder.expectResponse().status)

        // The paced fixture keeps the second read outstanding at cancel.
        call.read(ByteBuffer.allocateDirect(BUFFER_BYTES))
        assertTrue("the first piece arrives", recorder.expectRead() > 0)
        val outstanding = ByteBuffer.allocateDirect(BUFFER_BYTES)
        call.read(outstanding)
        call.cancel()

        val terminal = recorder.next()
        assertTrue(
            "a cancelled call ends with a failure, got ${describe(terminal)}",
            terminal is Event.Failed,
        )

        val marker = ByteArray(BUFFER_BYTES) { MARKER }
        outstanding.clear()
        outstanding.put(marker)
        outstanding.position(0)
        Thread.sleep(PACE_MS * PIECES_AFTER_CANCEL)

        val after = ByteArray(BUFFER_BYTES)
        outstanding.duplicate().get(after)
        assertArrayEquals("the buffer is untouched after the terminal callback", marker, after)
        val extra = recorder.next(QUIET_MS)
        assertTrue("nothing follows the terminal callback, got ${describe(extra)}", extra == null)
    }

    @Test
    fun aSecondCancelIsHarmless() {
        val recorder = CallRecorder()
        val call = OkHttpTransport(OkHttpClient())
            .start("GET", TestServerFixture.createBehavior(pacedBody()), NO_HEADERS, null, recorder)
        assertEquals(200, recorder.expectResponse().status)

        call.cancel()
        call.cancel()

        val terminal = recorder.next()
        assertTrue(
            "a cancelled call ends with a failure, got ${describe(terminal)}",
            terminal is Event.Failed,
        )
        val extra = recorder.next(QUIET_MS)
        assertTrue("the second cancel reports nothing, got ${describe(extra)}", extra == null)
    }

    @Test
    fun aCleartextRefusalFailsAtOnceAsPermanent() {
        val permitted = URL(TestServerFixture.signal(SMALL_SIGNAL))
        val refused = URL(permitted.protocol, REFUSED_HOST, permitted.port, permitted.file)

        val recorder = CallRecorder()
        val started = System.nanoTime()
        OkHttpTransport(OkHttpClient()).start("GET", refused.toString(), NO_HEADERS, null, recorder)
        val terminal = recorder.next(REFUSAL_MS)
        val elapsedMs = (System.nanoTime() - started) / 1_000_000

        assertTrue(
            "a policy refusal is a permanent failure, got ${describe(terminal)}",
            terminal is Event.Failed && terminal.permanent,
        )
        assertTrue("the refusal arrives in ${elapsedMs}ms", elapsedMs < REFUSAL_MS)
    }

    @Test
    fun aRedirectIsFollowedThoughTheHostClientDoesNot() {
        val target = TestServerFixture.signal(SMALL_SIGNAL)
        val host = OkHttpClient.Builder()
            .followRedirects(false)
            .addNetworkInterceptor { chain ->
                val response = chain.proceed(chain.request())
                if (chain.request().url.encodedPath.endsWith("/health")) {
                    response.close()
                    response.newBuilder()
                        .code(302)
                        .message("Found")
                        .header("Location", target)
                        .body("".toResponseBody(null))
                        .build()
                } else {
                    response
                }
            }
            .build()

        val recorder = CallRecorder()
        val call = OkHttpTransport(host)
            .start("GET", TestServerFixture.url("health"), NO_HEADERS, null, recorder)
        assertEquals(200, recorder.expectResponse().status)
        assertArrayEquals(URL(target).readBytes(), join(readBody(call, recorder)))
    }

    @Test
    fun theHostInterceptorAppliesAndItsCacheStaysEmpty() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val directory = File(context.cacheDir, "okhttp-host-cache")
        directory.deleteRecursively()
        val cache = Cache(directory, CACHE_BYTES)
        val sent = LinkedBlockingQueue<Headers>()
        val host = OkHttpClient.Builder()
            .cache(cache)
            .addInterceptor { chain ->
                chain.proceed(chain.request().newBuilder().header(HOST_HEADER, HOST_VALUE).build())
            }
            .addNetworkInterceptor { chain ->
                sent.put(chain.request().headers)
                chain.proceed(chain.request())
            }
            .build()

        try {
            val recorder = CallRecorder()
            val call = OkHttpTransport(host).start(
                "GET",
                TestServerFixture.signal(SMALL_SIGNAL),
                arrayOf(KITHARA_HEADER, KITHARA_VALUE),
                null,
                recorder,
            )
            assertEquals(200, recorder.expectResponse().status)
            readBody(call, recorder)

            val headers = sent.poll(TIMEOUT_MS, TimeUnit.MILLISECONDS)
                ?: throw AssertionError("the request never reached the network")
            assertEquals("the host interceptor's header is sent", HOST_VALUE, headers[HOST_HEADER])
            assertEquals("Kithara's header is sent as given", KITHARA_VALUE, headers[KITHARA_HEADER])
            assertEquals("the host cache serves no request", 0, cache.requestCount())
            assertEquals("the host cache stores nothing", 0L, cache.size())
        } finally {
            cache.close()
            directory.deleteRecursively()
        }
    }

    @Test
    fun aHeldStreamLeavesAnotherCallToTheSameHostFree() {
        val dispatcher = Dispatcher().apply {
            maxRequests = 1
            maxRequestsPerHost = 1
        }
        val transport = OkHttpTransport(OkHttpClient.Builder().dispatcher(dispatcher).build())

        val held = CallRecorder()
        val stream = transport
            .start("GET", TestServerFixture.signal(LARGE_SIGNAL), NO_HEADERS, null, held)
        assertEquals(200, held.expectResponse().status)
        stream.read(ByteBuffer.allocateDirect(BUFFER_BYTES))
        assertTrue("the held stream delivers its first read", held.expectRead() > 0)

        try {
            val recorder = CallRecorder()
            val call = transport
                .start("GET", TestServerFixture.signal(SMALL_SIGNAL), NO_HEADERS, null, recorder)
            assertEquals(200, recorder.expectResponse().status)
            assertTrue("the second call reads its body", join(readBody(call, recorder)).isNotEmpty())
        } finally {
            stream.cancel()
        }
    }
}

private const val LARGE_SIGNAL = "signal_mp3_sine440_60s.mp3"

private const val SMALL_SIGNAL = "signal_mp3_saw_1s.mp3"

// READ_SIZE of kithara-net's host backend.
private const val BUFFER_BYTES = 64 * 1024

private const val TIMEOUT_MS = 5_000L

private const val QUIET_MS = 500L

private const val REFUSAL_MS = 2_000L

// The test application's network security policy permits cleartext to 127.0.0.1 only.
private const val REFUSED_HOST = "localhost"

private const val PACE_MS = 400L

private const val PIECES_AFTER_CANCEL = 3L

private const val PACED_PIECE_BYTES = 512

private const val PACED_BODY_BYTES = 8 * 1024

private const val MARKER: Byte = 0x5A

private const val CACHE_BYTES = 1L shl 20

private const val HOST_HEADER = "X-Kithara-Host"

private const val HOST_VALUE = "host-interceptor"

private const val KITHARA_HEADER = "X-Kithara-Request"

private const val KITHARA_VALUE = "as-given"

private val NO_HEADERS = emptyArray<String>()

private fun pacedBody(): String {
    val bytes = ByteArray(PACED_BODY_BYTES) { (it % 251).toByte() }
    val base64 = Base64.getEncoder().encodeToString(bytes)
    return """{"content":{"kind":"bytes","base64":"$base64"},""" +
        """"delivery":{"kind":"throttle","chunk":$PACED_PIECE_BYTES,"delay_ms":$PACE_MS}}"""
}

private fun readBody(call: HttpCall, recorder: CallRecorder): List<ByteArray> {
    val chunks = mutableListOf<ByteArray>()
    while (true) {
        val buffer = ByteBuffer.allocateDirect(BUFFER_BYTES)
        call.read(buffer)
        val read = recorder.expectReadOrEnd() ?: return chunks
        val chunk = ByteArray(read)
        buffer.position(0)
        buffer.get(chunk)
        chunks += chunk
    }
}

private fun join(chunks: List<ByteArray>): ByteArray {
    val body = ByteArray(chunks.sumOf { it.size })
    var offset = 0
    for (chunk in chunks) {
        chunk.copyInto(body, offset)
        offset += chunk.size
    }
    return body
}

private class CallRecorder : HttpCallback {

    private val events = LinkedBlockingQueue<Event>()

    override fun onResponse(status: Int, headers: Array<String>) {
        events.put(Event.Response(status))
    }

    override fun onRead(bytes: Int) {
        events.put(Event.Read(bytes))
    }

    override fun onEnd() {
        events.put(Event.End)
    }

    override fun onFailed(message: String, permanent: Boolean) {
        events.put(Event.Failed(message, permanent))
    }

    fun next(timeoutMs: Long = TIMEOUT_MS): Event? = events.poll(timeoutMs, TimeUnit.MILLISECONDS)

    fun expectResponse(): Event.Response = when (val event = next()) {
        is Event.Response -> event
        else -> throw AssertionError("expected a response, got ${describe(event)}")
    }

    fun expectRead(): Int = expectReadOrEnd()
        ?: throw AssertionError("expected a read, got the end of the body")

    fun expectReadOrEnd(): Int? = when (val event = next()) {
        is Event.Read -> event.bytes
        Event.End -> null
        else -> throw AssertionError("expected a read or the end, got ${describe(event)}")
    }
}

private sealed interface Event {
    class Response(val status: Int) : Event

    class Read(val bytes: Int) : Event

    data object End : Event

    class Failed(val message: String, val permanent: Boolean) : Event
}

private fun describe(event: Event?): String = when (event) {
    null -> "nothing"
    is Event.Response -> "a response with status ${event.status}"
    is Event.Read -> "a read of ${event.bytes} bytes"
    Event.End -> "the end of the body"
    is Event.Failed -> "a failure (permanent=${event.permanent}): ${event.message}"
}
