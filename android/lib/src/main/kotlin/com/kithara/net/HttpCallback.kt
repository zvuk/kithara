package com.kithara.net

/** Kithara's side of one call; the transport calls it one callback at a time. */
interface HttpCallback {

    /**
     * Report the response, once per call and before the first read completes.
     *
     * @param status HTTP status code of the response.
     * @param headers response headers as flattened name and value pairs, less
     *   `Content-Encoding` and `Content-Length` of a body the host's client
     *   decoded.
     */
    fun onResponse(status: Int, headers: Array<String>)

    /** Answer the outstanding read with the [bytes] written into its buffer. */
    fun onRead(bytes: Int)

    /** Answer the outstanding read with the end of the body; this ends the call. */
    fun onEnd()

    /**
     * Report the failure that ends the call.
     *
     * @param message what failed, for the log and the error Kithara raises.
     * @param permanent true when asking again cannot succeed, such as a
     *   request the network security policy or the trust configuration
     *   refuses; Kithara then does not retry it.
     */
    fun onFailed(message: String, permanent: Boolean)
}
