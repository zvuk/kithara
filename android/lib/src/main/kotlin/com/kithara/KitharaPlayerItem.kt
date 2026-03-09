package com.kithara

import com.kithara.ffi.AudioPlayerItem
import com.kithara.ffi.FfiException
import com.kithara.ffi.ItemObserver
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * Queue item wrapper around the raw UniFFI `AudioPlayerItem`.
 *
 * Create an item, call [load], then insert it into [KitharaPlayer].
 *
 * @param url Source URL for the item.
 * @param additionalHeaders Optional request headers sent when loading the item.
 */
class KitharaPlayerItem(
    /**
     * Source URL for the item.
     */
    val url: String,
    additionalHeaders: Map<String, String>? = null,
) {
    internal val inner = AudioPlayerItem(url, additionalHeaders)

    /**
     * Stable identifier for the item instance.
     */
    val id: String = inner.id()

    private val observer = ItemObserverBridge(this)
    private val stateFlow = MutableStateFlow(ItemState())

    init {
        inner.setObserver(observer)
    }

    /**
     * Item state as a single observable snapshot.
     */
    val state: StateFlow<ItemState> = stateFlow.asStateFlow()

    /**
     * Current item status.
     */
    val status: ItemStatus
        get() = state.value.status

    /**
     * Current duration in seconds, if known.
     */
    val duration: Double?
        get() = state.value.duration

    /**
     * Buffered duration in seconds.
     */
    val bufferedDuration: Double
        get() = state.value.bufferedDuration

    /**
     * Last item error, if any.
     */
    val error: KitharaError?
        get() = state.value.error

    /**
     * Preferred peak bitrate in bits per second. Zero means no limit.
     */
    var preferredPeakBitrate: Double
        get() = inner.preferredPeakBitrate()
        set(value) {
            inner.setPreferredPeakBitrate(value)
        }

    /**
     * Preferred peak bitrate for expensive networks. Zero means no limit.
     */
    var preferredPeakBitrateForExpensiveNetworks: Double
        get() = inner.preferredPeakBitrateForExpensiveNetworks()
        set(value) {
            inner.setPreferredPeakBitrateForExpensiveNetworks(value)
        }

    /**
     * Asynchronously prepares the underlying Rust resource.
     *
     * Must complete successfully before the item is inserted into a player queue.
     */
    suspend fun load() {
        try {
            inner.load()
        } catch (error: FfiException) {
            throw KitharaError.fromFfi(error)
        }
    }

    private fun updateState(update: (ItemState) -> ItemState) {
        stateFlow.update(update)
    }

    private fun handleStatusChanged(code: Int) {
        updateState { current -> current.copy(status = itemStatusFromCode(code)) }
    }

    private fun handleDurationChanged(seconds: Double) {
        updateState { current -> current.copy(duration = seconds) }
    }

    private fun handleBufferedDurationChanged(seconds: Double) {
        updateState { current -> current.copy(bufferedDuration = seconds) }
    }

    private fun handleError(code: Int, message: String) {
        updateState { current ->
            current.copy(error = KitharaError.fromObserverCode(code, message))
        }
    }

    private class ItemObserverBridge(
        private val item: KitharaPlayerItem,
    ) : ItemObserver {
        override fun onDurationChanged(seconds: Double) {
            item.handleDurationChanged(seconds)
        }

        override fun onBufferedDurationChanged(seconds: Double) {
            item.handleBufferedDurationChanged(seconds)
        }

        override fun onStatusChanged(statusCode: Int) {
            item.handleStatusChanged(statusCode)
        }

        override fun onError(code: Int, message: String) {
            item.handleError(code, message)
        }
    }
}
