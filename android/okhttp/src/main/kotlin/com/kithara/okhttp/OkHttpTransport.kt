package com.kithara.okhttp

import com.kithara.net.HttpCall
import com.kithara.net.HttpCallback
import com.kithara.net.HttpTransport
import java.io.IOException
import java.nio.ByteBuffer
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.ThreadFactory
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock
import okhttp3.Call
import okhttp3.Headers
import okhttp3.MediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okio.BufferedSink
import okio.BufferedSource

/**
 * [HttpTransport] over the host's [OkHttpClient], and the reference behavior
 * of the protocol stated on [HttpTransport].
 *
 * Shares the host client's pool and interceptors, skips its cache, follows
 * redirects and runs calls on its own threads.
 *
 * @param client the host's client, which the transport derives its own from.
 */
class OkHttpTransport(client: OkHttpClient) : HttpTransport {

    private val client: OkHttpClient = client.newBuilder()
        .cache(null)
        .followRedirects(true)
        .followSslRedirects(true)
        .build()

    private val calls: ExecutorService = Executors.newCachedThreadPool(CallThreads())

    override fun start(
        method: String,
        url: String,
        headers: Array<String>,
        body: ByteBuffer?,
        callback: HttpCallback,
    ): HttpCall {
        val transfer = Transfer(method, url, headers, body, callback)
        calls.execute(transfer)
        return transfer
    }

    private inner class Transfer(
        private val method: String,
        private val url: String,
        private val headers: Array<String>,
        private val body: ByteBuffer?,
        private val callback: HttpCallback,
    ) : HttpCall, Runnable {

        private val lock = ReentrantLock()
        private val readable = lock.newCondition()
        private var pending: ByteBuffer? = null
        private var cancelled = false
        private var call: Call? = null

        // Only the call's thread touches it: it alone runs callbacks.
        private var ended = false

        override fun read(buffer: ByteBuffer) {
            lock.withLock {
                pending = buffer
                readable.signalAll()
            }
        }

        override fun cancel() {
            val active = lock.withLock {
                cancelled = true
                readable.signalAll()
                call
            }
            active?.cancel()
        }

        override fun run() {
            // A request OkHttp refuses to build fails the same way every time.
            val request = try {
                request()
            } catch (error: IllegalArgumentException) {
                return fail(error.toString(), permanent = true)
            }
            try {
                val call = open(request) ?: return fail("the call was cancelled")
                call.execute().use { response ->
                    callback.onResponse(response.code, flatten(response.headers))
                    val source = response.body?.source()
                        ?: return fail("the response carries no body")
                    transfer(source)
                }
            } catch (error: IOException) {
                fail(error.toString(), isPermanent(error))
            } catch (error: RuntimeException) {
                fail(error.toString())
            } catch (error: InterruptedException) {
                Thread.currentThread().interrupt()
                fail(error.toString())
            }
        }

        private fun request(): Request {
            val request = Request.Builder().url(url)
            for (index in 0 until headers.size - 1 step 2) {
                request.addHeader(headers[index], headers[index + 1])
            }
            request.method(method, body?.let(::BufferBody))
            return request.build()
        }

        private fun open(request: Request): Call? = lock.withLock {
            if (cancelled) null else client.newCall(request).also { call = it }
        }

        private fun transfer(source: BufferedSource) {
            while (true) {
                val buffer = awaitRead() ?: return fail("the call was cancelled")
                val read = source.read(buffer)
                if (read < 0) {
                    return end()
                }
                callback.onRead(read)
            }
        }

        private fun awaitRead(): ByteBuffer? = lock.withLock {
            while (pending == null && !cancelled) {
                readable.await()
            }
            val buffer = pending
            pending = null
            if (cancelled) null else buffer
        }

        private fun end() {
            if (ended) return
            ended = true
            callback.onEnd()
        }

        private fun fail(message: String, permanent: Boolean = false) {
            if (ended) return
            ended = true
            callback.onFailed(message, permanent)
        }
    }

    private class BufferBody(private val body: ByteBuffer) : RequestBody() {

        override fun contentType(): MediaType? = null

        override fun contentLength(): Long = body.remaining().toLong()

        override fun writeTo(sink: BufferedSink) {
            sink.write(body.duplicate())
        }
    }

    private class CallThreads : ThreadFactory {

        private val next = AtomicInteger()

        override fun newThread(body: Runnable): Thread =
            Thread(body, "kithara-okhttp-${next.getAndIncrement()}").apply { isDaemon = true }
    }
}

private fun flatten(headers: Headers): Array<String> =
    Array(headers.size * 2) { index ->
        if (index % 2 == 0) headers.name(index / 2) else headers.value(index / 2)
    }
