package com.kithara

/**
 * Top-level Android configuration for the Kithara runtime.
 *
 * @property store Storage and cache options applied during initialization.
 */
data class KitharaConfig(
    val store: KitharaStoreOptions = KitharaStoreOptions(),
)
