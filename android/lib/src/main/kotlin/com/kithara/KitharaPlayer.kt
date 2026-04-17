package com.kithara

import com.kithara.ffi.AudioPlayer as FfiAudioPlayer
import com.kithara.ffi.FfiException
import com.kithara.ffi.FfiKeyOptions
import com.kithara.ffi.FfiKeyProcessor
import com.kithara.ffi.FfiKeyRule
import com.kithara.ffi.FfiPlayerConfig
import com.kithara.ffi.FfiPlayerEvent
import com.kithara.ffi.FfiPlayerStatus
import com.kithara.ffi.FfiTrackStatus
import com.kithara.ffi.FfiTransition
import com.kithara.ffi.PlayerObserver
import com.kithara.ffi.SeekCallback
import com.kithara.ffi.StoreOptions as FfiStoreOptions
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
class KitharaPlayer(config: Config = Config()) {
    /**
     * A single DRM rule: a key processor bound to one or more domain
     * patterns (exact `"example.com"` or wildcard `"*.example.com"`),
     * plus optional headers / query params sent with matching key
     * requests.
     */
    data class KeyRule(
        val processor: KeyProcessor,
        val domains: List<String>,
        val headers: Map<String, String>? = null,
        val queryParams: Map<String, String>? = null,
    )

    /** Configuration for [KitharaPlayer] creation. */
    data class Config(
        val eqBandCount: Int = 10,
        val keyRules: List<KeyRule> = emptyList(),
        val cacheDir: String? = null,
    )

    private val inner: FfiAudioPlayer = FfiAudioPlayer(config.toFfi())
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

    /**
     * Select an item at the given queue index.
     *
     * @param index Queue index (0-based).
     * @param transition How the switch plays. [Transition.None] is an
     *   immediate cut (AVQueuePlayer user-initiated-selection idiom —
     *   default). [Transition.Crossfade] uses the player's configured
     *   duration (typical for Next/Prev buttons).
     * @throws KitharaError If the index is out of range or the item is
     *   not yet loaded.
     */
    @Throws(KitharaError::class)
    fun selectItem(at: Int, transition: Transition = Transition.None) {
        try {
            inner.selectItem(at.toUInt(), transition.toFfi())
        } catch (error: FfiException) {
            throw KitharaError.fromFfi(error)
        }
    }

    /**
     * Select an item by identity (AVQueuePlayer-style).
     *
     * Race-free against concurrent insert/remove that would shift
     * indices — resolves the item's current index on the fly.
     *
     * @param item Item to select; must currently be in the queue.
     * @param transition `.None` by default (immediate cut); pass
     *   `.Crossfade` for Next/Prev button UX.
     * @throws KitharaError If the item is not in the queue, or the
     *   underlying select fails.
     */
    @Throws(KitharaError::class)
    fun selectItem(item: KitharaPlayerItem, transition: Transition = Transition.None) {
        val idx = items.indexOfFirst { queued -> queued.id == item.id }
        if (idx < 0) {
            throw KitharaError.InvalidArgument("item ${item.id} not in queue")
        }
        selectItem(at = idx, transition = transition)
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

            is FfiPlayerEvent.TrackStatusChanged ->
                eventsFlow.tryEmit(
                    KitharaPlayerEvent.TrackStatusChanged(
                        event.itemId,
                        event.status.toTrackStatus(),
                    )
                )

            is FfiPlayerEvent.QueueEnded ->
                eventsFlow.tryEmit(KitharaPlayerEvent.QueueEnded)

            is FfiPlayerEvent.TimeControlStatusChanged,
            is FfiPlayerEvent.VolumeChanged,
            is FfiPlayerEvent.MuteChanged,
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

private fun FfiTrackStatus.toTrackStatus(): TrackStatus = when (this) {
    is FfiTrackStatus.Pending -> TrackStatus.Pending
    is FfiTrackStatus.Loading -> TrackStatus.Loading
    is FfiTrackStatus.Slow -> TrackStatus.Slow
    is FfiTrackStatus.Loaded -> TrackStatus.Loaded
    is FfiTrackStatus.Failed -> TrackStatus.Failed(reason)
    is FfiTrackStatus.Consumed -> TrackStatus.Consumed
}

private fun Transition.toFfi(): FfiTransition = when (this) {
    is Transition.None -> FfiTransition.None
    is Transition.Crossfade -> FfiTransition.Crossfade
    is Transition.CrossfadeWith -> FfiTransition.CrossfadeWith(seconds)
}

/**
 * Callback for processing (decrypting) HLS encryption keys.
 */
fun interface KeyProcessor {
    fun processKey(key: ByteArray): ByteArray
}

private class KeyProcessorBridge(private val processor: KeyProcessor) : FfiKeyProcessor {
    override fun processKey(key: ByteArray): ByteArray = processor.processKey(key)
}

private fun KitharaPlayer.Config.toFfi(): FfiPlayerConfig {
    val ffiRules = keyRules.map { rule ->
        FfiKeyRule(
            processor = KeyProcessorBridge(rule.processor),
            domains = rule.domains,
            headers = rule.headers,
            queryParams = rule.queryParams,
        )
    }
    return FfiPlayerConfig(
        eqBandCount = eqBandCount.toUInt(),
        keyOptions = FfiKeyOptions(rules = ffiRules),
        store = FfiStoreOptions(cacheDir = cacheDir),
    )
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
