package com.kithara

/**
 * Storage options passed to the native cache layer.
 *
 * @property cacheDir Optional cache directory path. When omitted, `Kithara.initialize`
 * uses `Context.getCacheDir()`.
 */
data class KitharaStoreOptions(
    val cacheDir: String? = null,
)
