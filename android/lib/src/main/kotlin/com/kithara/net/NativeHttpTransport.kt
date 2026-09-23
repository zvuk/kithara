package com.kithara.net

/**
 * Hands the application's [HttpTransport] to the native library.
 *
 * The native-test harness calls it from Java, which sees Kotlin `internal` as public.
 */
internal object NativeHttpTransport {

    /**
     * Install [transport] for every request in the process.
     *
     * @throws RuntimeException when the process already has a transport.
     */
    @JvmStatic
    external fun install(transport: HttpTransport)
}
