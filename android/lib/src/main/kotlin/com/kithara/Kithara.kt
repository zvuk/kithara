package com.kithara

import android.content.Context

/**
 * Entry point for the Kithara audio engine.
 *
 * Call [initialize] once before using any Kithara API — typically in
 * `Application.onCreate`. After that, create players and items directly:
 *
 * ```kotlin
 * // In Application.onCreate:
 * Kithara.initialize(applicationContext)
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
     */
    fun initialize(context: Context) {
        if (ready) return
        synchronized(this) {
            if (ready) return
            System.loadLibrary("kithara_ffi")
            nativeInitRustlsPlatformVerifier(context.applicationContext)
            cacheDir = context.applicationContext.cacheDir
                .resolve("kithara")
                .absolutePath
            ready = true
        }
    }

    @JvmStatic
    private external fun nativeInitRustlsPlatformVerifier(context: Context)
}
