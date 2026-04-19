package com.kithara

import android.content.Context

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
 * // In Application.onCreate:
 * Kithara.initialize(applicationContext, logLevel = LogLevel.Debug)
 *
 * // Anywhere in the app:
 * val player = KitharaPlayer()
 * val item = KitharaPlayerItem("https://example.com/audio.mp3")
 * lifecycleScope.launch {
 *     item.load()
 *     player.insert(item)
 *     player.play()
 * }
 * ```
 */
object Kithara {
    /**
     * Cache directory path used by all items created after [initialize].
     * Empty string if [initialize] has not been called yet.
     */
    @Volatile
    internal var cacheDir: String = ""
        private set

    @Volatile
    private var ready = false

    /**
     * Initialize the native Kithara library.
     *
     * Must be called once before creating any [KitharaPlayer] or [KitharaPlayerItem].
     * Safe to call multiple times — subsequent calls are no-ops.
     *
     * @param context Any [Context]; the application context is used internally.
     * @param logLevel Minimum log level forwarded from Rust to logcat. Defaults to [LogLevel.Warn].
     */
    fun initialize(context: Context, logLevel: LogLevel = LogLevel.Warn) {
        if (ready) return
        synchronized(this) {
            if (ready) return
            System.loadLibrary("kithara_ffi")
            nativeInit(context.applicationContext, logLevel.ordinal)
            cacheDir = context.applicationContext.cacheDir
                .resolve("kithara")
                .absolutePath
            ready = true
        }
    }

    @JvmStatic
    private external fun nativeInit(context: Context, logLevel: Int)

    @JvmStatic
    private external fun nativeRunOfflineCapture(
        inputPath: String,
        outputPath: String,
        seconds: Int,
    ): Long

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
         * Requires [initialize] to have been called first. Must run before
         * any [KitharaPlayer] is constructed in the same process — the
         * underlying offline backend initialises a process-wide singleton
         * and panics if a non-offline backend was installed earlier.
         *
         * @return `0` on success; non-zero error code otherwise.
         */
        fun runOfflineCapture(inputPath: String, outputPath: String, seconds: Int): Long {
            return nativeRunOfflineCapture(inputPath, outputPath, seconds)
        }

        /**
         * Enumerate cpal default host / output device and log every supported
         * output config. Returns the default sample format code (see
         * [SampleFormat]) or a negative error code.
         *
         * Used to verify that the format the firewheel graph produces
         * (interleaved f32 stereo) matches what cpal negotiates with the
         * Android audio backend — a mismatch implies a lossy conversion
         * happens inside cpal before samples reach AAudio.
         */
        fun probeAndroidAudio(): Long = nativeProbeAndroidAudio()

        /**
         * Codes mirrored from `FMT_*` constants in `android_test.rs`.
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
            const val ERR_NO_DEVICE: Long = -1
            const val ERR_DEFAULT_CFG: Long = -2
            const val ERR_SUPPORTED_CFGS: Long = -3

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
                ERR_NO_DEVICE -> "ERR_NO_DEVICE"
                ERR_DEFAULT_CFG -> "ERR_DEFAULT_CFG"
                ERR_SUPPORTED_CFGS -> "ERR_SUPPORTED_CFGS"
                else -> "UNKNOWN($code)"
            }
        }
    }
}
