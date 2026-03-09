package com.kithara

import com.kithara.ffi.AudioPlayer
import com.kithara.ffi.FfiException
import com.kithara.ffi.PlayerObserver
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * Queue-based Android player facade over the raw `AudioPlayer`.
 *
 * Public consumers can either observe the aggregated [state] or read the
 * convenience accessors for the latest snapshot.
 */
class KitharaPlayer {
    private val inner = AudioPlayer()
    private val observer = PlayerObserverBridge(this)
    private val currentItemChangesFlow = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
    private val stateFlow = MutableStateFlow(PlayerState())

    init {
        inner.setObserver(observer)
    }

    /**
     * Full player state as a single observable snapshot.
     */
    val state: StateFlow<PlayerState> = stateFlow.asStateFlow()

    /**
     * One-shot event emitted when the current item changes.
     */
    val currentItemChanges: SharedFlow<Unit> = currentItemChangesFlow.asSharedFlow()

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
     * @param seconds Target playback position in seconds from the item start.
     * @throws KitharaError If the native seek request fails.
     */
    @Throws(KitharaError::class)
    fun seek(seconds: Double) {
        try {
            inner.seek(seconds)
        } catch (error: FfiException) {
            throw KitharaError.fromFfi(error)
        }
    }

    /**
     * Inserts an item into the queue.
     *
     * The item must already be prepared via [KitharaPlayerItem.load].
     *
     * @param item Prepared item to insert.
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

    private fun handleTimeChanged(seconds: Double) {
        updateState { current -> current.copy(currentTime = seconds) }
    }

    private fun handleRateChanged(rate: Float) {
        updateState { current -> current.copy(rate = rate) }
    }

    private fun handleCurrentItemChanged() {
        currentItemChangesFlow.tryEmit(Unit)
    }

    private fun handleStatusChanged(code: Int) {
        updateState { current -> current.copy(status = playerStatusFromCode(code)) }
    }

    private fun handleError(code: Int, message: String) {
        updateState { current ->
            current.copy(error = KitharaError.fromObserverCode(code, message))
        }
    }

    private fun handleDurationChanged(seconds: Double) {
        updateState { current -> current.copy(duration = seconds) }
    }

    private fun handleBufferedDurationChanged(seconds: Double) {
        updateState { current -> current.copy(bufferedDuration = seconds) }
    }

    private class PlayerObserverBridge(
        private val player: KitharaPlayer,
    ) : PlayerObserver {
        override fun onTimeChanged(seconds: Double) {
            player.handleTimeChanged(seconds)
        }

        override fun onRateChanged(rate: Float) {
            player.handleRateChanged(rate)
        }

        override fun onCurrentItemChanged() {
            player.handleCurrentItemChanged()
        }

        override fun onStatusChanged(statusCode: Int) {
            player.handleStatusChanged(statusCode)
        }

        override fun onError(code: Int, message: String) {
            player.handleError(code, message)
        }

        override fun onDurationChanged(seconds: Double) {
            player.handleDurationChanged(seconds)
        }

        override fun onBufferedDurationChanged(seconds: Double) {
            player.handleBufferedDurationChanged(seconds)
        }
    }
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
