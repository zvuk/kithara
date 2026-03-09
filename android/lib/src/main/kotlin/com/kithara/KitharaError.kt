package com.kithara

import com.kithara.ffi.FfiException

/**
 * Public Android error type mapped from the Rust FFI layer.
 */
sealed class KitharaError(message: String) : Exception(message) {
    /**
     * Operation requires a prepared item or ready player state.
     */
    data object NotReady : KitharaError("player not ready")

    /**
     * Item playback or loading failed.
     *
     * @property reason Human-readable failure reason.
     */
    data class ItemFailed(val reason: String) : KitharaError(reason)

    /**
     * Seek request failed.
     *
     * @property reason Human-readable failure reason.
     */
    data class SeekFailed(val reason: String) : KitharaError(reason)

    /**
     * Player engine is not running.
     */
    data object EngineNotRunning : KitharaError("engine not running")

    /**
     * Supplied argument is invalid for the requested operation.
     *
     * @property reason Human-readable validation error.
     */
    data class InvalidArgument(val reason: String) : KitharaError(reason)

    /**
     * Unexpected internal failure propagated from the native layer.
     *
     * @property description Human-readable error description.
     */
    data class Internal(val description: String) : KitharaError(description)

    companion object {
        internal fun fromFfi(error: FfiException): KitharaError = when (error) {
            is FfiException.NotReady -> NotReady
            is FfiException.ItemFailed -> ItemFailed(error.reason)
            is FfiException.SeekFailed -> SeekFailed(error.reason)
            is FfiException.EngineNotRunning -> EngineNotRunning
            is FfiException.InvalidArgument -> InvalidArgument(error.reason)
            is FfiException.Internal -> Internal(error.description)
        }

        internal fun fromObserverCode(code: Int, message: String): KitharaError = when (code) {
            1 -> NotReady
            2 -> ItemFailed(message)
            3 -> SeekFailed(message)
            4 -> EngineNotRunning
            5 -> InvalidArgument(message)
            else -> Internal(message)
        }
    }
}
