package com.kithara.net

import java.nio.ByteBuffer

/**
 * The HTTP transport Kithara runs every request through on Android.
 *
 * An implementation:
 * 1. Returns at once from every method Kithara calls.
 * 2. Calls [HttpCallback.onResponse] once per call, for any HTTP status and
 *    after following redirects, before the first read completes.
 * 3. Gets one [HttpCall.read] at a time and answers it with
 *    [HttpCallback.onRead] or [HttpCallback.onEnd], writing the buffer only
 *    until that answer.
 * 4. Ends each call with exactly one [HttpCallback.onEnd] or
 *    [HttpCallback.onFailed], a cancelled call included, and touches no
 *    buffer Kithara lent it afterwards.
 * 5. Reports a failure through [HttpCallback.onFailed], marking it
 *    permanent when asking again cannot succeed; Kithara retries only the
 *    others.
 * 6. Leaves trust, proxies, cookies, timeouts, pooling and content coding to
 *    the host's client.
 *
 * Two callbacks of one call never overlap.
 */
interface HttpTransport {

    /**
     * Start one request without blocking and return its handle.
     *
     * @param method HTTP method in upper case: `GET`, `HEAD` or `POST`.
     * @param url absolute request URL.
     * @param headers request headers as flattened name and value pairs, to
     *   send as given.
     * @param body request body of a `POST`, a direct buffer readable until the
     *   terminal callback; null for every other method.
     * @param callback receives the response, every read and the end of the
     *   call.
     */
    fun start(
        method: String,
        url: String,
        headers: Array<String>,
        body: ByteBuffer?,
        callback: HttpCallback,
    ): HttpCall
}
