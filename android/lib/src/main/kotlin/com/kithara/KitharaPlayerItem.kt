package com.kithara

import com.kithara.ffi.AudioPlayerItem as FfiAudioPlayerItem
import com.kithara.ffi.FfiItemEvent
import com.kithara.ffi.FfiItemStatus
import com.kithara.ffi.ItemObserver
import com.kithara.ffi.StoreOptions
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * A single audio item that can be queued in [KitharaPlayer].
 *
 * Call [load] to start preparing the resource, then insert the item into a player queue.
 * Errors during load are delivered via [state] as [ItemState.error].
 * Call [Kithara.initialize] before creating any instance.
 *
 * ```kotlin
 * val item = KitharaPlayerItem("https://example.com/audio.mp3")
 * item.load()
 * player.insert(item)
 * player.play()
 * ```
 *
 * @param url The audio source URL.
 * @param additionalHeaders Optional HTTP headers included in all requests for this item.
 */
class KitharaPlayerItem(
    url: String,
    additionalHeaders: Map<String, String>? = null,
) {
    internal val inner: FfiAudioPlayerItem = FfiAudioPlayerItem(url, additionalHeaders)

    init {
        inner.setStoreOptions(StoreOptions(cacheDir = Kithara.cacheDir))
    }

    /**
     * Source URL for this item.
     */
    val url: String = inner.url()

    /**
     * Stable identifier for this item instance.
     */
    val id: String = inner.id()

    private val observer = ItemObserverBridge(this)
    private val stateFlow = MutableStateFlow(ItemState())

    init {
        inner.setObserver(observer)
    }

    /**
     * Full item state as a single observable snapshot.
     */
    val state: StateFlow<ItemState> = stateFlow.asStateFlow()

    /**
     * Current loading state of the item.
     */
    val status: ItemStatus
        get() = state.value.status

    /**
     * Duration in seconds, or `null` if unknown.
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
     * Preferred peak bitrate for expensive networks in bits per second. Zero means no limit.
     */
    var preferredPeakBitrateForExpensiveNetworks: Double
        get() = inner.preferredPeakBitrateForExpensiveNetworks()
        set(value) {
            inner.setPreferredPeakBitrateForExpensiveNetworks(value)
        }

    /**
     * Starts loading the underlying resource (fire-and-forget).
     *
     * Safe to call before inserting into a [KitharaPlayer] — the call is idempotent.
     * Errors are reported through [state] as [ItemState.error].
     */
    fun load() {
        inner.load()
    }

    private fun updateState(update: (ItemState) -> ItemState) {
        stateFlow.update(update)
    }

    private fun handleEvent(event: FfiItemEvent) {
        when (event) {
            is FfiItemEvent.DurationChanged ->
                updateState { it.copy(duration = event.seconds) }

            is FfiItemEvent.BufferedDurationChanged ->
                updateState { it.copy(bufferedDuration = event.seconds) }

            is FfiItemEvent.StatusChanged ->
                updateState { it.copy(status = event.status.toItemStatus()) }

            is FfiItemEvent.Error ->
                updateState { it.copy(
                    error = KitharaError.ItemFailed(event.error),
                    status = ItemStatus.Failed,
                ) }
        }
    }

    private class ItemObserverBridge(
        private val item: KitharaPlayerItem,
    ) : ItemObserver {
        override fun onEvent(event: FfiItemEvent) {
            item.handleEvent(event)
        }
    }
}

private fun FfiItemStatus.toItemStatus(): ItemStatus = when (this) {
    FfiItemStatus.READY_TO_PLAY -> ItemStatus.ReadyToPlay
    FfiItemStatus.FAILED -> ItemStatus.Failed
    FfiItemStatus.UNKNOWN -> ItemStatus.Unknown
}
