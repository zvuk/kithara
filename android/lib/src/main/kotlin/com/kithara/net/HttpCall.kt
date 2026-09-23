package com.kithara.net

import java.nio.ByteBuffer

/** One request an [HttpTransport] started. */
interface HttpCall {

    /**
     * Fill [buffer] from its position towards its limit, then answer with
     * [HttpCallback.onRead] or, at the end of the body, [HttpCallback.onEnd].
     */
    fun read(buffer: ByteBuffer)

    /**
     * Stop the call from any thread. Idempotent; an unfinished call still ends
     * with one terminal callback.
     */
    fun cancel()
}
