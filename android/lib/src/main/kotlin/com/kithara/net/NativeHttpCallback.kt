package com.kithara.net

/** The [HttpCallback] the native library constructs for each call. */
internal class NativeHttpCallback(private val handle: Long) : HttpCallback {

    override fun onResponse(status: Int, headers: Array<String>) =
        nativeOnResponse(handle, status, headers)

    override fun onRead(bytes: Int) = nativeOnRead(handle, bytes)

    override fun onEnd() = nativeOnEnd(handle)

    override fun onFailed(message: String, permanent: Boolean) =
        nativeOnFailed(handle, message, permanent)

    companion object {
        @JvmStatic
        external fun nativeOnResponse(handle: Long, status: Int, headers: Array<String>)

        @JvmStatic
        external fun nativeOnRead(handle: Long, bytes: Int)

        @JvmStatic
        external fun nativeOnEnd(handle: Long)

        @JvmStatic
        external fun nativeOnFailed(handle: Long, message: String, permanent: Boolean)
    }
}
