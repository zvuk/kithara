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
}
