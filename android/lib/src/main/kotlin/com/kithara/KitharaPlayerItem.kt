package com.kithara

import com.kithara.ffi.AudioPlayerItem as FfiAudioPlayerItem
import com.kithara.ffi.FfiAbrMode
import com.kithara.ffi.FfiItemConfig
import com.kithara.ffi.FfiItemEvent
import com.kithara.ffi.FfiItemStatus
import com.kithara.ffi.ItemObserver
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * A single audio item that can be queued in [KitharaPlayer].
 *
 * All preferences (bitrate caps, ABR mode, headers) are frozen at
 * construction. Loading starts automatically when the item is inserted
 * into a [KitharaPlayer].
 *
 * ```kotlin
 * val item = KitharaPlayerItem("https://example.com/audio.mp3")
 * player.insert(item)
 * player.play()
 * ```
 *
 * @param url The audio source URL.
 * @param additionalHeaders Optional HTTP headers included in all requests for this item.
 * @param preferredPeakBitrate Peak bitrate ceiling in bits/sec. `0.0` means no cap.
 * @param preferredPeakBitrateForExpensiveNetworks Peak bitrate ceiling on
 *   expensive networks (cellular). `0.0` means no cap.
 * @param abrMode Optional per-item ABR mode override.
 */
class KitharaPlayerItem(
    url: String,
    additionalHeaders: Map<String, String>? = null,
    preferredPeakBitrate: Double = 0.0,
    preferredPeakBitrateForExpensiveNetworks: Double = 0.0,
    abrMode: FfiAbrMode? = null,
) {
    internal val inner: FfiAudioPlayerItem = FfiAudioPlayerItem(
        FfiItemConfig(
            url = url,
            headers = additionalHeaders,
            preferredPeakBitrate = preferredPeakBitrate,
            preferredPeakBitrateExpensive = preferredPeakBitrateForExpensiveNetworks,
            abrMode = abrMode,
        )
    )

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
    val preferredPeakBitrate: Double
        get() = inner.preferredPeakBitrate()

    /**
     * Preferred peak bitrate for expensive networks in bits per second. Zero means no limit.
     */
    val preferredPeakBitrateForExpensiveNetworks: Double
        get() = inner.preferredPeakBitrateForExpensiveNetworks()

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

            is FfiItemEvent.VariantsDiscovered,
            is FfiItemEvent.VariantSelected,
            is FfiItemEvent.VariantApplied -> Unit

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

