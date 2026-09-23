package com.kithara

import androidx.test.platform.app.InstrumentationRegistry
import java.net.HttpURLConnection
import java.net.URL

/**
 * Client of the hermetic fixture server the harness starts on the host. The
 * base URL arrives as an instrumentation runner argument, pointing at a device
 * port an `adb reverse` mapping forwards to the host.
 */
object TestServerFixture {

    const val ARGUMENT = "KITHARA_TEST_SERVER_URL"

    class FixtureException(message: String, cause: Throwable? = null) :
        IllegalStateException(message, cause)

    /** @throws FixtureException when the argument is absent or is not an HTTP URL. */
    fun requireBaseUrl(): String =
        parseBaseUrl(InstrumentationRegistry.getArguments().getString(ARGUMENT))

    /** @throws FixtureException when [value] is absent or is not an HTTP URL. */
    fun parseBaseUrl(value: String?): String {
        if (value.isNullOrBlank()) {
            throw FixtureException(
                "$ARGUMENT is unset. The harness passes it as " +
                    "-Pandroid.testInstrumentationRunnerArguments.$ARGUMENT=<url>.",
            )
        }
        val url = try {
            URL(value)
        } catch (error: java.net.MalformedURLException) {
            throw FixtureException("$ARGUMENT is not a URL: $value", error)
        }
        if (url.protocol != "http" && url.protocol != "https") {
            throw FixtureException("$ARGUMENT is not an HTTP URL: $value")
        }
        if (url.host.isNullOrEmpty()) {
            throw FixtureException("$ARGUMENT carries no host: $value")
        }
        return value.trimEnd('/')
    }

    fun url(path: String): String = "${requireBaseUrl()}/${path.trimStart('/')}"

    /** @throws FixtureException when the server does not answer `ok`. */
    fun requireHealthy() {
        val body = get(url("health"))
        if (body.trim() != "ok") {
            throw FixtureException("health endpoint answered `$body`")
        }
    }

    fun signal(nameWithExtension: String): String = url("signal/$nameWithExtension")

    /** @throws FixtureException when the server rejects the specification. */
    fun createHls(spec: String): String {
        val token = token(post(url("token"), "{\"hls_spec\":$spec}"), "token")
        return url("stream/$token.m3u8")
    }

    /** @throws FixtureException when the server rejects the specification. */
    fun createBehavior(spec: String): String {
        val token = token(post(url("control/behavior"), spec), "control/behavior")
        return url("behavior/$token")
    }

    private fun token(answer: String, endpoint: String): String {
        val token = answer
            .substringAfter("\"token\":\"", "")
            .substringBefore('"', "")
        if (token.isEmpty()) {
            throw FixtureException("$endpoint endpoint returned no token")
        }
        return token
    }

    private fun get(endpoint: String): String = request(endpoint) { }

    private fun post(endpoint: String, body: String): String = request(endpoint) { connection ->
        connection.requestMethod = "POST"
        connection.doOutput = true
        connection.setRequestProperty("Content-Type", "application/json")
        connection.outputStream.use { it.write(body.toByteArray()) }
    }

    private fun request(
        endpoint: String,
        configure: (HttpURLConnection) -> Unit,
    ): String {
        val connection = URL(endpoint).openConnection() as HttpURLConnection
        connection.connectTimeout = TIMEOUT_MS
        connection.readTimeout = TIMEOUT_MS
        try {
            configure(connection)
            val status = connection.responseCode
            if (status !in 200..299) {
                val detail = connection.errorStream?.bufferedReader()?.use { it.readText() }
                throw FixtureException("$endpoint answered HTTP $status: $detail")
            }
            return connection.inputStream.bufferedReader().use { it.readText() }
        } catch (error: java.io.IOException) {
            throw FixtureException("$endpoint is unreachable", error)
        } finally {
            connection.disconnect()
        }
    }

    private const val TIMEOUT_MS = 10_000
}
