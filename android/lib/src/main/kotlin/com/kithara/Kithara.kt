package com.kithara

import android.content.Context
import com.kithara.net.HttpTransport
import com.kithara.net.NativeHttpTransport

/**
 * Minimum log level forwarded from the Rust layer to logcat.
 *
 * Maps to `tracing` levels: [Trace] is the most verbose, [Off] disables all logging.
 */
enum class LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Off,
}

/**
 * Entry point for the Kithara audio engine.
 *
 * Call [initialize] once before using any Kithara API — typically in
 * `Application.onCreate`. After that, create players and items directly:
 *
 * ```kotlin
 * // In Application.onCreate, with the adapter from `kithara-okhttp`:
 * Kithara.initialize(applicationContext, OkHttpTransport(okHttpClient), logLevel = LogLevel.Debug)
 *
 * // Anywhere in the app:
 * val player = KitharaPlayer()
 * val item = KitharaPlayerItem("https://example.com/audio.mp3")
 * lifecycleScope.launch {
 *     player.insert(item)
 *     player.play()
 * }
 * ```
 */
object Kithara {
    /**
     * Process-wide asset store created by [initialize].
     *
     * The same native store is shared by every default-configured player.
     * Access before [initialize] throws [IllegalStateException].
     */
    @Volatile
    private var initializedStore: AssetStore? = null

    private var installedTransport: HttpTransport? = null

    val defaultStore: AssetStore
        get() = checkNotNull(initializedStore) {
            "Kithara.initialize must be called before accessing the default asset store"
        }

    /**
     * Initialize the native Kithara library.
     *
     * Must be called before creating any [KitharaPlayer] or [KitharaPlayerItem].
     * The process keeps the transport of the first call: a later call with the
     * same transport changes nothing, and a later call with another transport
     * throws.
     *
     * @param context Any [Context]; the application context is used internally.
     * @param transport The HTTP transport every request runs through; `kithara-okhttp` provides one.
     * @param logLevel Minimum log level forwarded from Rust to logcat. Defaults to [LogLevel.Warn].
     * @throws IllegalStateException when an earlier call installed another transport.
     */
    fun initialize(context: Context, transport: HttpTransport, logLevel: LogLevel = LogLevel.Warn) {
        synchronized(this) {
            val installed = installedTransport
            if (installed != null) {
                check(installed === transport) {
                    "Kithara is already initialized with another HttpTransport"
                }
                return
            }
            System.loadLibrary("kithara_ffi")
            nativeInit(context.applicationContext, logLevel.ordinal)
            NativeHttpTransport.install(transport)
            installedTransport = transport
            initializedStore = AssetStore(
                root = context.applicationContext.cacheDir
                    .resolve("kithara")
                    .absolutePath,
            )
        }
    }

    @JvmStatic
    private external fun nativeInit(context: Context, logLevel: Int)

    @JvmStatic
    private external fun nativeRunOfflineCapture(
        inputPath: String,
        outputPath: String,
        seconds: Int,
    )

    @JvmStatic
    private external fun nativeProbeAndroidAudio(): Long

    /**
     * Diagnostic entry points available only in debug builds of the library.
     *
     * Gated by the Rust `test` feature: in release AARs the symbol is absent
     * and [Test.runOfflineCapture] throws `UnsatisfiedLinkError` when invoked.
     */
    object Test {
        /**
         * Render `seconds` of audio from [inputPath] through the offline
         * firewheel backend and write an IEEE-float WAV to [outputPath].
         *
         * Bypasses cpal / AAudio — used to localise Android-only audio
         * artefacts. A clean WAV implicates the output path; a distorted
         * WAV implicates the decoder / graph compiled for Android.
         *
         * Requires [initialize] to have been called first.
         *
         * @throws RuntimeException naming the step that failed.
         */
        fun runOfflineCapture(inputPath: String, outputPath: String, seconds: Int) {
            nativeRunOfflineCapture(inputPath, outputPath, seconds)
        }

        /**
         * Enumerate cpal default host / output device and log every supported
         * output config. Returns the default sample format code (see
         * [SampleFormat]) and throws when the device cannot be inspected.
         *
         * Used to verify that the format the firewheel graph produces
         * (interleaved f32 stereo) matches what cpal negotiates with the
         * Android audio backend — a mismatch implies a lossy conversion
         * happens inside cpal before samples reach AAudio.
         */
        fun probeAndroidAudio(): Long = nativeProbeAndroidAudio()

        /**
         * Codes mirrored from the `FMT_*` constants of the native probe.
         * Keep in sync with the Rust side.
         */
        object SampleFormat {
            const val F32: Long = 0
            const val I16: Long = 1
            const val U16: Long = 2
            const val I8: Long = 3
            const val I32: Long = 4
            const val I64: Long = 5
            const val U8: Long = 6
            const val U32: Long = 7
            const val U64: Long = 8
            const val F64: Long = 9
            const val OTHER: Long = 10

            fun name(code: Long): String = when (code) {
                F32 -> "F32"
                I16 -> "I16"
                U16 -> "U16"
                I8 -> "I8"
                I32 -> "I32"
                I64 -> "I64"
                U8 -> "U8"
                U32 -> "U32"
                U64 -> "U64"
                F64 -> "F64"
                OTHER -> "OTHER"
                else -> "UNKNOWN($code)"
            }
        }
    }
}
