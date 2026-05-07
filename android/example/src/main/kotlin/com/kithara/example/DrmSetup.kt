package com.kithara.example

import android.content.Context
import com.kithara.ffi.FfiCipher

/**
 * Read the cipher key for zvuk DRM from the bundled `.env` asset.
 *
 * The salt is supplied per-call by the player (see
 * `KitharaPlayer.setupHlsAes`) — the demo no longer pre-generates a
 * seed; the closure builds the cipher on every decrypt from
 * `cipherKey + salt`.
 */
internal fun readZvukCipherKey(context: Context): String =
    readEnvValue(context, key = "DRM_KEY") ?: "BinaryCipherKey"

internal fun readZvukAuthToken(context: Context): String? =
    readEnvValue(context, key = "KITHARA_DRM_AUTH_TOKEN")

/**
 * Decrypt `encryptedKey` with a one-shot kithara cipher built from
 * `secret`. Used by the wildcard `setupHlsAes` decryptor closure.
 */
internal fun kitharaCipherDecrypt(secret: String, encryptedKey: ByteArray): ByteArray {
    val cipher = FfiCipher(secret)
    return cipher.decrypt(encryptedKey)
}

private fun readEnvValue(context: Context, key: String): String? {
    val candidates = listOf(".env", "env")
    for (name in candidates) {
        val value = runCatching {
            context.assets.open(name).bufferedReader().useLines { lines ->
                lines
                    .map { it.trim() }
                    .firstOrNull { line -> line.startsWith("$key=") }
                    ?.substringAfter('=')
                    ?.trim()
            }
        }.getOrNull()
        if (value != null) return value
    }
    return null
}
