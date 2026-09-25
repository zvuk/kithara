package com.kithara

import com.kithara.ffi.AudioPlayerItem as FfiAudioPlayerItem
import com.kithara.ffi.FfiAbrMode
import com.kithara.ffi.FfiItemConfig
import com.kithara.ffi.FfiItemEvent
import com.kithara.ffi.FfiItemLoadResult
import com.kithara.ffi.FfiItemStatus
import com.kithara.ffi.FfiSourceSettings
import com.kithara.ffi.FfiTimeRange
import com.kithara.ffi.FfiVariant
import com.kithara.ffi.ItemLoadCallback
import com.kithara.ffi.ItemObserver
import kotlin.coroutines.resume
import kotlin.coroutines.suspendCoroutine
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.buffer
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.mapNotNull
import kotlinx.coroutines.flow.onStart

private fun itemConfig(
    url: String,
    additionalHeaders: Map<String, String>?,
    preferredPeakBitrate: Double,
    preferredPeakBitrateForExpensiveNetworks: Double,
    abrMode: FfiAbrMode?,
    isLiveStream: Boolean,
    audioId: ULong?,
    uuid: Long?,
): FfiItemConfig = FfiItemConfig(
    abrMode = abrMode,
    audioId = audioId,
    headers = additionalHeaders,
    uuidI64 = uuid,
    url = url,
    preferredPeakBitrate = preferredPeakBitrate,
    preferredPeakBitrateExpensive = preferredPeakBitrateForExpensiveNetworks,
    isLiveStream = isLiveStream,
)

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
class KitharaPlayerItem internal constructor(
    internal val inner: FfiAudioPlayerItem,
) {
    constructor(
        url: String,
        additionalHeaders: Map<String, String>? = null,
        preferredPeakBitrate: Double = 0.0,
        preferredPeakBitrateForExpensiveNetworks: Double = 0.0,
        abrMode: FfiAbrMode? = null,
        isLiveStream: Boolean = false,
        audioId: ULong? = null,
        uuid: Long? = null,
    ) : this(
        FfiAudioPlayerItem(
            itemConfig(
                abrMode = abrMode,
                audioId = audioId,
                additionalHeaders = additionalHeaders,
                uuid = uuid,
                url = url,
                preferredPeakBitrate = preferredPeakBitrate,
                preferredPeakBitrateForExpensiveNetworks = preferredPeakBitrateForExpensiveNetworks,
                isLiveStream = isLiveStream,
            )
        )
    )

    /** Create an item with generated source settings validated before loading. */
    constructor(
        url: String,
        additionalHeaders: Map<String, String>? = null,
        preferredPeakBitrate: Double = 0.0,
        preferredPeakBitrateForExpensiveNetworks: Double = 0.0,
        abrMode: FfiAbrMode? = null,
        isLiveStream: Boolean = false,
        audioId: ULong? = null,
        uuid: Long? = null,
        sourceSettings: FfiSourceSettings,
    ) : this(
        FfiAudioPlayerItem.newWithSourceSettings(
            config = itemConfig(
                abrMode = abrMode,
                audioId = audioId,
                additionalHeaders = additionalHeaders,
                uuid = uuid,
                url = url,
                preferredPeakBitrate = preferredPeakBitrate,
                preferredPeakBitrateForExpensiveNetworks = preferredPeakBitrateForExpensiveNetworks,
                isLiveStream = isLiveStream,
            ),
            settings = sourceSettings,
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

    private val queueId: ULong = inner.queueId()

    /** Two wrappers are equal when they stand for the same queued item. */
    override fun equals(other: Any?): Boolean =
        other is KitharaPlayerItem && other.queueId == queueId

    override fun hashCode(): Int = queueId.hashCode()

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

    /**
     * Item state, emitted on every change while collected. The first
     * value is the state as it stands when collection starts.
     */
    val state: Flow<ItemState>
        get() = ffiEvents
            .filter { it.changesState() }
            .map { snapshot() }
            .onStart { emit(snapshot()) }
            .distinctUntilChanged()

    /** One-shot item events. */
    val events: Flow<KitharaItemEvent>
        get() = ffiEvents.mapNotNull { it.toKitharaItemEvent() }

    val status: ItemStatus
        get() = snapshot().status

    val duration: Double?
        get() = snapshot().duration

    /** Buffered ranges (start + duration in seconds). */
    val loadedRanges: List<ItemLoadedRange>
        get() = snapshot().loadedRanges

    val error: KitharaError?
        get() = snapshot().error

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

    /**
     * Cold: each collector registers its own observer on the item. The
     * native event thread never waits on a collector; a slow collector
     * loses the oldest buffered events.
     */
    private val ffiEvents: Flow<FfiItemEvent> = callbackFlow {
        val observer = object : ItemObserver {
            override fun onEvent(event: FfiItemEvent) {
                trySend(event)
            }
        }
        val id = inner.addObserver(observer)
        awaitClose { inner.removeObserver(id) }
    }.buffer(EVENT_BUFFER, BufferOverflow.DROP_OLDEST)

    private fun FfiItemEvent.toKitharaItemEvent(): KitharaItemEvent? = when (this) {
        is FfiItemEvent.DurationChanged -> KitharaItemEvent.DurationChanged(seconds)
        is FfiItemEvent.VariantsDiscovered -> KitharaItemEvent.VariantsDiscovered(variants.map { it.toKitharaVariant() })
        is FfiItemEvent.VariantSelected -> KitharaItemEvent.VariantSelected(variant.toKitharaVariant())
        is FfiItemEvent.VariantApplied -> KitharaItemEvent.VariantApplied(variant.toKitharaVariant())
        is FfiItemEvent.Error -> KitharaItemEvent.Error(error)
        else -> null
    }

    private companion object {
        const val EVENT_BUFFER = 64
    }

    private fun snapshot(): ItemState {
        val state = inner.state()
        return ItemState(
            loadedRanges = state.loadedRanges.map {
                ItemLoadedRange(start = it.startSeconds, duration = it.durationSeconds)
            },
            duration = state.durationSeconds,
            error = state.error?.let { KitharaError.ItemFailed(it) },
            status = state.status.toItemStatus(),
        )
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

private fun FfiItemEvent.changesState(): Boolean = when (this) {
    is FfiItemEvent.StatusChanged,
    is FfiItemEvent.DurationChanged,
    is FfiItemEvent.LoadedRangesChanged,
    is FfiItemEvent.DidFail,
    is FfiItemEvent.Error -> true
    else -> false
}

private fun FfiVariant.toKitharaVariant(): KitharaVariant =
    KitharaVariant(index, bandwidthBps.toLong(), name)

private fun FfiItemStatus.toItemStatus(): ItemStatus = when (this) {
    FfiItemStatus.READY_TO_PLAY -> ItemStatus.ReadyToPlay
    FfiItemStatus.FAILED -> ItemStatus.Failed
    FfiItemStatus.UNKNOWN -> ItemStatus.Unknown
}
