package com.kithara

import com.kithara.ffi.AudioPlayer as FfiAudioPlayer
import com.kithara.ffi.FfiException
import com.kithara.ffi.FfiPlayerConfig
import com.kithara.ffi.FfiPlayerEvent
import com.kithara.ffi.FfiPlayerStatus
import com.kithara.ffi.PlayerObserver
import com.kithara.ffi.SeekCallback
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * Queue-based audio player.
 *
 * Observe [state] for reactive updates, or read convenience accessors for the
 * latest snapshot. Call [Kithara.initialize] before creating any instance.
 *
 * ```kotlin
 * val player = KitharaPlayer()
 * val item = KitharaPlayerItem("https://example.com/audio.mp3")
 * item.load()
 * player.insert(item)
 * player.play()
 * ```
 */
class KitharaPlayer() {
    private val inner: FfiAudioPlayer = FfiAudioPlayer(FfiPlayerConfig(eqBandCount = 10u))
    private val observer = PlayerObserverBridge(this)
    private val eventsFlow = MutableSharedFlow<KitharaPlayerEvent>(extraBufferCapacity = 16)
    private val stateFlow = MutableStateFlow(PlayerState())

    init {
        inner.setObserver(observer)
    }

    /**
     * Full player state as a single observable snapshot.
     */
    val state: StateFlow<PlayerState> = stateFlow.asStateFlow()

    /**
     * One-shot player events: item changes, playback completion, and failures.
     *
     * Subscribe with [kotlinx.coroutines.flow.collect] to react to lifecycle transitions
     * without polling [state].
     */
    val events: SharedFlow<KitharaPlayerEvent> = eventsFlow.asSharedFlow()

    /**
     * Current playback time in seconds.
     */
    val currentTime: Double
        get() = state.value.currentTime

    /**
     * Current playback rate.
     */
    val rate: Float
        get() = state.value.rate

    /**
     * Current player status.
     */
    val status: PlayerStatus
        get() = state.value.status

    /**
     * Current item duration in seconds, if known.
     */
    val duration: Double?
        get() = state.value.duration

    /**
     * Current buffered duration in seconds.
     */
    val bufferedDuration: Double
        get() = state.value.bufferedDuration

    /**
     * Last player error, if any.
     */
    val error: KitharaError?
        get() = state.value.error

    /**
     * Current queue snapshot.
     */
    val items: List<KitharaPlayerItem>
        get() = state.value.items

    /**
     * Default playback rate used by [play].
     */
    var defaultRate: Float
        get() = inner.defaultRate()
        set(value) {
            inner.setDefaultRate(value)
        }

    /**
     * Starts or resumes playback.
     */
    fun play() {
        inner.play()
    }

    /**
     * Pauses playback.
     */
    fun pause() {
        inner.pause()
    }

    /**
     * Seeks the current item to the given position in seconds.
     *
     * Result is delivered asynchronously via [onComplete].
     *
     * @param seconds Target playback position in seconds from the item start.
     * @param onComplete Called with `true` if seek succeeded, `false` otherwise.
     */
    fun seek(seconds: Double, onComplete: (Boolean) -> Unit) {
        inner.seek(seconds, SeekCallbackAdapter(onComplete))
    }

    /**
     * Inserts an item into the queue.
     *
     * @param item Item to insert.
     * @param after Optional queue item after which the new item should be inserted.
     * @throws KitharaError If the native insert request fails.
     */
    @Throws(KitharaError::class)
    fun insert(item: KitharaPlayerItem, after: KitharaPlayerItem? = null) {
        try {
            inner.insert(item.inner, after?.inner)
            updateState { current ->
                current.copy(items = current.items.inserted(item, after))
            }
        } catch (error: FfiException) {
            throw KitharaError.fromFfi(error)
        }
    }

    /**
     * Removes an item from the queue.
     *
     * @param item Queue item to remove.
     * @throws KitharaError If the native remove request fails.
     */
    @Throws(KitharaError::class)
    fun remove(item: KitharaPlayerItem) {
        try {
            inner.remove(item.inner)
            updateState { current ->
                current.copy(items = current.items.filterNot { queued -> queued.id == item.id })
            }
        } catch (error: FfiException) {
            throw KitharaError.fromFfi(error)
        }
    }

    /**
     * Clears the queue.
     */
    fun removeAllItems() {
        inner.removeAllItems()
        updateState { current -> current.copy(items = emptyList()) }
    }

    private fun updateState(update: (PlayerState) -> PlayerState) {
        stateFlow.update(update)
    }

    private fun handleEvent(event: FfiPlayerEvent) {
        when (event) {
            is FfiPlayerEvent.TimeChanged ->
                updateState { it.copy(currentTime = event.seconds) }

            is FfiPlayerEvent.RateChanged ->
                updateState { it.copy(rate = event.rate) }

            is FfiPlayerEvent.DurationChanged ->
                updateState { it.copy(duration = event.seconds) }

            is FfiPlayerEvent.BufferedDurationChanged ->
                updateState { it.copy(bufferedDuration = event.seconds) }

            is FfiPlayerEvent.StatusChanged ->
                updateState { it.copy(status = event.status.toPlayerStatus()) }

            is FfiPlayerEvent.Error ->
                updateState { it.copy(error = KitharaError.Internal(event.error)) }

            is FfiPlayerEvent.CurrentItemChanged ->
                eventsFlow.tryEmit(KitharaPlayerEvent.CurrentItemChanged(event.itemId))

            is FfiPlayerEvent.ItemDidPlayToEnd ->
                eventsFlow.tryEmit(KitharaPlayerEvent.PlayedToEnd)

            is FfiPlayerEvent.TimeControlStatusChanged,
            is FfiPlayerEvent.VolumeChanged,
            is FfiPlayerEvent.MuteChanged,
            is FfiPlayerEvent.TrackStatusChanged,
            is FfiPlayerEvent.QueueEnded,
            is FfiPlayerEvent.CrossfadeStarted,
            is FfiPlayerEvent.CrossfadeDurationChanged -> Unit
        }
    }

    private class PlayerObserverBridge(
        private val player: KitharaPlayer,
    ) : PlayerObserver {
        override fun onEvent(event: FfiPlayerEvent) {
            player.handleEvent(event)
        }
    }

    private class SeekCallbackAdapter(
        private val callback: (Boolean) -> Unit,
    ) : SeekCallback {
        override fun onComplete(finished: Boolean) {
            callback(finished)
        }
    }
}

private fun FfiPlayerStatus.toPlayerStatus(): PlayerStatus = when (this) {
    FfiPlayerStatus.READY_TO_PLAY -> PlayerStatus.ReadyToPlay
    FfiPlayerStatus.FAILED -> PlayerStatus.Failed
    FfiPlayerStatus.UNKNOWN -> PlayerStatus.Unknown
}

private fun List<KitharaPlayerItem>.inserted(
    item: KitharaPlayerItem,
    after: KitharaPlayerItem?,
): List<KitharaPlayerItem> = buildList {
    addAll(this@inserted)
    if (after == null) {
        add(item)
        return@buildList
    }

    val index = indexOfFirst { queued -> queued.id == after.id }
    if (index >= 0) {
        add(index + 1, item)
    } else {
        add(item)
    }
}
