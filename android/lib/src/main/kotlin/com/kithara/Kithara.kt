package com.kithara

import android.content.Context

/**
 * Entry point for initializing the Android bindings.
 *
 * Example:
 * ```kotlin
 * import androidx.lifecycle.lifecycleScope
 * import kotlinx.coroutines.launch
 *
 * Kithara.initialize(applicationContext)
 *
 * val player = KitharaPlayer()
 * val item = KitharaPlayerItem("https://example.com/audio.mp3")
 *
 * lifecycleScope.launch {
 *     item.load()
 *     player.insert(item)
 *     player.play()
 * }
 * ```
 */
object Kithara {
    @Volatile
    private var initialized = false

    /**
     * Loads the native library and configures shared runtime state.
     *
     * Safe to call multiple times; repeated calls after the first one are ignored.
     *
     * @param context Android context used to resolve application directories.
     * @param config Android-side configuration forwarded to the native layer.
     */
    fun initialize(
        context: Context,
        config: KitharaConfig = KitharaConfig(),
    ) {
        if (initialized) {
            return
        }

        synchronized(this) {
            if (initialized) {
                return
            }

            System.loadLibrary("kithara_ffi")
            nativeSetStoreOptions(
                config.store.copy(
                    cacheDir = config.store.cacheDir ?: context.cacheDir.absolutePath,
                ),
            )
            nativeInitRustlsPlatformVerifier(context.applicationContext)
            initialized = true
        }
    }

    @JvmStatic
    private external fun nativeSetStoreOptions(store: KitharaStoreOptions)

    @JvmStatic
    private external fun nativeInitRustlsPlatformVerifier(context: Context)
}
