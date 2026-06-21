package com.kithara

import com.kithara.ffi.AudioPlayerItem as FfiAudioPlayerItem
import com.kithara.ffi.FfiAbrMode
import com.kithara.ffi.FfiItemConfig
import com.kithara.ffi.FfiItemEvent
import com.kithara.ffi.FfiItemLoadResult
import com.kithara.ffi.FfiItemStatus
import com.kithara.ffi.FfiTimeRange
import com.kithara.ffi.ItemLoadCallback
import com.kithara.ffi.ItemObserver
import kotlin.coroutines.resume
import kotlin.coroutines.suspendCoroutine
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
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
 */
class KitharaPlayerItem(
    url: String,
    additionalHeaders: Map<String, String>? = null,
    preferredPeakBitrate: Double = 0.0,
    preferredPeakBitrateForExpensiveNetworks: Double = 0.0,
    abrMode: FfiAbrMode? = null,
    isLiveStream: Boolean = false,
    audioId: ULong? = null,
    uuid: Long? = null,
) {
    internal val inner: FfiAudioPlayerItem = FfiAudioPlayerItem(
        FfiItemConfig(
            abrMode = abrMode,
            audioId = audioId,
            headers = additionalHeaders,
            uuidI64 = uuid,
            url = url,
            preferredPeakBitrate = preferredPeakBitrate,
            preferredPeakBitrateExpensive = preferredPeakBitrateForExpensiveNetworks,
            isLiveStream = isLiveStream,
        )
    )

    /** Source URL for this item. */
    val url: String = inner.url()

    /**
     * Stable per-item identifier: the decimal string form of the
     * monotonic `TrackId` (`u64`) the queue reserves at construction.
     * Mirrors the iOS `AudioPlayerItemProtocol.audioId`. Synonym alias
     * [id] is kept for `Identifiable`-style consumers.
     */
    val audioId: String = inner.audioId().toString()

    /** Synonym for [audioId]. */
    val id: String get() = audioId

    /** Numeric form of [audioId] derived from the first 16 hex digits. */
    val uuid: Long
        get() = inner.uuidI64()

    /**
     * Caller-declared live-stream flag. Mirrors the iOS
     * `AudioPlayerItemProtocol.isLiveStream`.
     */
    val isLiveStream: Boolean
        get() = inner.isLiveStream()

    /**
     * Cached duration in seconds. Defaults to `0.0` until the
     * underlying resource emits a duration update.
     */
    val durationSec: Double
        get() = inner.durationSec()

    private val observer = ItemObserverBridge(this)
    private val stateFlow = MutableStateFlow(ItemState())
    private val eventsFlow = MutableSharedFlow<KitharaItemEvent>(extraBufferCapacity = 16)

    init {
        inner.setObserver(observer)
    }

    /** Full item state as a single observable snapshot. */
    val state: StateFlow<ItemState> = stateFlow.asStateFlow()

    /** One-shot item events. */
    val events: SharedFlow<KitharaItemEvent> = eventsFlow.asSharedFlow()

    val status: ItemStatus
        get() = state.value.status

    val duration: Double?
        get() = state.value.duration

    /** Buffered ranges (start + duration in seconds). */
    val loadedRanges: List<ItemLoadedRange>
        get() = state.value.loadedRanges

    val error: KitharaError?
        get() = state.value.error

    /** Preferred peak bitrate in bits per second. Zero means no limit. */
    val preferredPeakBitrate: Double
        get() = inner.preferredPeakBitrate()

    /** Preferred peak bitrate on expensive networks. Zero means no limit. */
    val preferredPeakBitrateForExpensiveNetworks: Double
        get() = inner.preferredPeakBitrateForExpensiveNetworks()

    /**
     * Resolve a snapshot of the current load status. The result
     * reflects cached state — `KitharaPlayer.insert` already starts
     * background loading. Mirrors the iOS `func load() -> Observable<…>`
     * via Kotlin coroutines.
     */
    suspend fun load(): ItemLoadResult = suspendCoroutine { cont ->
        inner.load(object : ItemLoadCallback {
            override fun onComplete(result: FfiItemLoadResult) {
                cont.resume(
                    ItemLoadResult(
                        hasProtectedContent = result.hasProtectedContent,
                        isPlayable = result.isPlayable,
                    )
                )
            }
        })
    }

    /**
     * Whether the item is playable at `progress` (seconds) given the
     * caller-supplied buffered `ranges`. Live streams are reported
     * playable unconditionally.
     */
    fun isPlayable(progress: Double, ranges: List<ItemLoadedRange>): Boolean =
        inner.isPlayable(
            progress,
            ranges.map { FfiTimeRange(durationSeconds = it.duration, startSeconds = it.start) },
        )

    private fun updateState(update: (ItemState) -> ItemState) {
        stateFlow.update(update)
    }

    private fun handleEvent(event: FfiItemEvent) {
        when (event) {
            is FfiItemEvent.DurationChanged -> {
                updateState { it.copy(duration = event.seconds) }
                eventsFlow.tryEmit(KitharaItemEvent.DurationChanged(event.seconds))
            }

            is FfiItemEvent.LoadedRangesChanged ->
                updateState {
                    it.copy(
                        loadedRanges = event.ranges.map { range ->
                            ItemLoadedRange(start = range.startSeconds, duration = range.durationSeconds)
                        }
                    )
                }

            is FfiItemEvent.StatusChanged ->
                updateState { it.copy(status = event.status.toItemStatus()) }

            is FfiItemEvent.VariantsDiscovered -> {
                val mapped = event.variants.map { KitharaVariant(it.index, it.bandwidthBps.toLong(), it.name) }
                eventsFlow.tryEmit(KitharaItemEvent.VariantsDiscovered(mapped))
            }

            is FfiItemEvent.VariantSelected ->
                eventsFlow.tryEmit(KitharaItemEvent.VariantSelected(KitharaVariant(event.variant.index, event.variant.bandwidthBps.toLong(), event.variant.name)))

            is FfiItemEvent.VariantApplied ->
                eventsFlow.tryEmit(KitharaItemEvent.VariantApplied(KitharaVariant(event.variant.index, event.variant.bandwidthBps.toLong(), event.variant.name)))

            is FfiItemEvent.DidReachEnd,
            is FfiItemEvent.DidStall -> Unit

            is FfiItemEvent.DidFail -> {
                updateState { it.copy(
                    error = KitharaError.ItemFailed("item did fail"),
                    status = ItemStatus.Failed,
                ) }
                eventsFlow.tryEmit(KitharaItemEvent.Error("item did fail"))
            }

            is FfiItemEvent.Error -> {
                updateState { it.copy(
                    error = KitharaError.ItemFailed(event.error),
                    status = ItemStatus.Failed,
                ) }
                eventsFlow.tryEmit(KitharaItemEvent.Error(event.error))
            }
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

/** Outcome reported by [KitharaPlayerItem.load]. */
data class ItemLoadResult(
    val hasProtectedContent: Boolean,
    val isPlayable: Boolean,
)

/** Buffered range expressed as `[start, start + duration)` seconds. */
data class ItemLoadedRange(
    val start: Double,
    val duration: Double,
)

private fun FfiItemStatus.toItemStatus(): ItemStatus = when (this) {
    FfiItemStatus.READY_TO_PLAY -> ItemStatus.ReadyToPlay
    FfiItemStatus.FAILED -> ItemStatus.Failed
    FfiItemStatus.UNKNOWN -> ItemStatus.Unknown
}
