package com.kithara.example.ui.player

import android.app.Application
import android.util.Log
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.kithara.AssetStore
import com.kithara.CrossfadeSettings
import com.kithara.Kithara
import com.kithara.KitharaError
import com.kithara.KitharaItemEvent
import com.kithara.KitharaPlayer
import com.kithara.KitharaPlayerEvent
import com.kithara.KitharaPlayerItem
import com.kithara.LogLevel
import com.kithara.PlayerStatus
import com.kithara.TrackStatus
import com.kithara.Transition
import com.kithara.example.drm.ZvukKeyProcessor
import com.kithara.example.drm.readZvukAuthToken
import com.kithara.example.drm.readZvukCipherKey
import com.kithara.ffi.FfiAbrMode
import com.kithara.okhttp.OkHttpTransport
import java.io.File
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import okhttp3.OkHttpClient

internal class PlayerViewModel(application: Application) : AndroidViewModel(application) {
    private val _uiState = MutableStateFlow(PlayerUiState())
    val uiState: StateFlow<PlayerUiState> = _uiState.asStateFlow()

    private val player: KitharaPlayer = createPlayer(application)

    /**
     * Per-item event subscriptions keyed by [KitharaPlayerItem.id]. Variant
     * discovery flows through here; queue-level events flow through
     * [observeEvents].
     */
    private val itemSubscriptions: MutableMap<String, Job> = mutableMapOf()

    init {
        _uiState.update {
            it.copy(
                volume = player.volume,
                isMuted = player.isMuted,
                crossfadeDuration = player.crossfadeSettings.duration,
                eqGains = List(EQ_BAND_COUNT) { band -> player.getEqGain(band) },
            )
        }

        observePlayer()
        observeEvents()

        for (url in DEFAULT_TRACK_URLS) {
            enqueue(url, autoPlay = false)
        }
    }

    fun onEvent(event: PlayerScreenEvent) {
        when (event) {
            is PlayerScreenEvent.UrlChanged -> onUrlChanged(event.url)
            PlayerScreenEvent.AddClick -> addTrack()
            PlayerScreenEvent.PlayPauseClick -> playPause()
            PlayerScreenEvent.PrevClick -> playPrev()
            PlayerScreenEvent.NextClick -> playNext()
            is PlayerScreenEvent.TrackClick -> selectTrack(event.trackId)
            is PlayerScreenEvent.RateClick -> setRate(event.rate)
            PlayerScreenEvent.SeekStarted -> onSeekStarted()
            is PlayerScreenEvent.SeekChanged -> onSeekChanged(event.value)
            PlayerScreenEvent.SeekFinished -> onSeekFinished()
            is PlayerScreenEvent.RemoveTrackClick -> removeTrack(event.trackId)
            is PlayerScreenEvent.VolumeChanged -> setVolume(event.volume)
            PlayerScreenEvent.MuteClick -> toggleMute()
            is PlayerScreenEvent.EqBandChanged -> setEqGain(event.bandIndex, event.gain)
            PlayerScreenEvent.EqResetClick -> resetEq()
            is PlayerScreenEvent.CrossfadeChanged -> setCrossfadeDuration(event.durationSeconds)
            is PlayerScreenEvent.AbrChanged -> setAbrMode(event.variantIndex)
        }
    }

    override fun onCleared() {
        super.onCleared()
        itemSubscriptions.values.forEach { it.cancel() }
        itemSubscriptions.clear()
        player.pause()
        player.removeAllItems()
    }

    private fun onUrlChanged(value: String) {
        _uiState.update { it.copy(url = value, errorMessage = null) }
    }

    private fun addTrack() {
        val url = _uiState.value.url.trim()
        if (url.isEmpty()) {
            setLocalError("Enter an audio URL or pick a file")
            return
        }

        _uiState.update { it.copy(url = "", errorMessage = null) }
        enqueue(url)
    }

    private fun playPause() {
        val state = _uiState.value
        if (state.isPlaying) {
            player.pause()
            return
        }

        if (state.currentTrackId == null) {
            state.playlist.firstOrNull()?.let { selectTrack(it.id) }
            return
        }

        player.play()
    }

    private fun playNext() {
        val state = _uiState.value
        val current = state.currentTrackIndex
        if (current < 0 || current >= state.playlist.lastIndex) return
        // Next button -> crossfade, symmetric with auto-advance at track end.
        switchTo(state.playlist[current + 1].id, Transition.Crossfade)
    }

    private fun playPrev() {
        val state = _uiState.value
        val current = state.currentTrackIndex
        if (current <= 0) return
        switchTo(state.playlist[current - 1].id, Transition.Crossfade)
    }

    private fun selectTrack(trackId: String) {
        // Tap on a track in the list -> immediate cut.
        switchTo(trackId, Transition.None)
    }

    private fun setRate(rate: Float) {
        _uiState.update { it.copy(selectedRate = rate) }
        player.playingRate = rate
        if (_uiState.value.isPlaying) player.play()
    }

    private fun setVolume(volume: Float) {
        player.volume = volume
        _uiState.update { it.copy(volume = volume) }
    }

    private fun toggleMute() {
        val muted = !player.isMuted
        player.isMuted = muted
        _uiState.update { it.copy(isMuted = muted) }
    }

    private fun setEqGain(band: Int, gainDb: Float) {
        player.setEqGain(band, gainDb)
        _uiState.update { state ->
            state.copy(
                eqGains = state.eqGains.mapIndexed { index, gain ->
                    if (index == band) gainDb else gain
                },
            )
        }
    }

    private fun resetEq() {
        player.resetEq()
        _uiState.update { it.copy(eqGains = List(EQ_BAND_COUNT) { 0f }) }
    }

    private fun setCrossfadeDuration(duration: Float) {
        player.crossfadeSettings = player.crossfadeSettings.copy(duration = duration)
        _uiState.update { it.copy(crossfadeDuration = duration) }
    }

    private fun setAbrMode(variantIndex: UInt?) {
        player.setAbrMode(variantIndex?.let(FfiAbrMode::Manual) ?: FfiAbrMode.Auto)
        _uiState.update {
            it.copy(
                abrIsAuto = variantIndex == null,
                selectedVariantIndex = variantIndex,
            )
        }
    }

    private fun removeTrack(trackId: String) {
        itemFor(trackId)?.let(player::remove)
        itemSubscriptions.remove(trackId)?.cancel()
        _uiState.update { state ->
            state.copy(playlist = state.playlist.filterNot { it.id == trackId })
        }
    }

    private fun onSeekStarted() {
        _uiState.update { it.copy(isSeeking = true) }
    }

    private fun onSeekChanged(value: Float) {
        _uiState.update { it.copy(currentTimeSeconds = value) }
    }

    private fun onSeekFinished() {
        val target = _uiState.value.currentTimeSeconds.toDouble()
        player.seek(target, tolerance = null) { ok ->
            if (ok) {
                _uiState.update { it.copy(isSeeking = false) }
                return@seek
            }
            Log.e(TAG, "Seek failed")
            _uiState.update { it.copy(errorMessage = "Seek failed", isSeeking = false) }
        }
    }

    private fun createPlayer(application: Application): KitharaPlayer {
        Kithara.initialize(
            application,
            OkHttpTransport(OkHttpClient()),
            logLevel = LogLevel.Debug,
        )

        // `filesDir` rather than `cacheDir` because kithara runs its own
        // eviction — the OS-managed `cacheDir` can be cleared under storage
        // pressure, which would desync our bookkeeping.
        val cacheDir = File(application.filesDir, CACHE_DIR_NAME)
            .apply { mkdirs() }
            .absolutePath

        // The wildcard HLS-AES key rule, the auth token and the demo
        // crossfade window are all initial state, so they are declared in
        // the configuration rather than set after construction.
        return KitharaPlayer(
            config = KitharaPlayer.Config(
                store = AssetStore(root = cacheDir),
                keyRules = listOf(
                    KitharaPlayer.KeyRule.wildcard(
                        ZvukKeyProcessor(readZvukCipherKey(application))
                    )
                ),
                authToken = readZvukAuthToken(application).orEmpty(),
                crossfadeSettings = CrossfadeSettings(duration = DEFAULT_CROSSFADE_SECONDS),
                playingRate = _uiState.value.selectedRate,
            ),
        )
    }

    private fun itemFor(trackId: String): KitharaPlayerItem? =
        player.items.firstOrNull { it.id == trackId }

    private fun enqueue(
        url: String,
        name: String = resolveTrackTitle(url),
        autoPlay: Boolean = true,
    ) {
        val item = KitharaPlayerItem(url)
        try {
            player.append(item)
        } catch (e: KitharaError) {
            Log.e(TAG, "Failed to insert: $url", e)
            setLocalError(e.reason())
            return
        }

        subscribeItem(item)

        val entry = PlaylistEntry(id = item.id, name = name, url = url)
        val wasEmpty = _uiState.value.playlist.isEmpty()
        _uiState.update { it.copy(playlist = it.playlist + entry) }

        if (autoPlay && wasEmpty) {
            switchTo(entry.id, Transition.None)
        }
    }

    private fun subscribeItem(item: KitharaPlayerItem) {
        // Cancel any previous subscription for the same id so we never
        // leak a worker if the same track is re-inserted.
        itemSubscriptions.remove(item.id)?.cancel()
        val itemId = item.id
        itemSubscriptions[itemId] = viewModelScope.launch {
            item.events.collect { event -> handleItemEvent(itemId, event) }
        }
    }

    private fun switchTo(trackId: String, transition: Transition) {
        val item = itemFor(trackId)
        if (item == null) {
            setLocalError("item $trackId not in queue")
            return
        }
        try {
            player.selectItem(item, transition)
            player.play()
        } catch (e: KitharaError) {
            Log.e(TAG, "Failed to select: $trackId", e)
            setLocalError(e.reason())
            return
        }
        // `status` is left as-is — the engine only emits StatusChanged on real
        // transitions, so touching it here makes the header flicker to
        // "Not Ready" between tracks.
        _uiState.update {
            it.copy(
                currentTrackId = trackId,
                currentTimeSeconds = 0f,
                durationSeconds = null,
                errorMessage = null,
                isSeeking = false,
            )
        }
    }

    private fun observePlayer() {
        viewModelScope.launch {
            player.state.collectLatest { state ->
                _uiState.update { current ->
                    val time = if (current.isSeeking) {
                        current.currentTimeSeconds
                    } else {
                        state.currentTime.toFloat()
                    }
                    val duration = state.duration?.toFloat()
                    current.copy(
                        currentTimeSeconds = duration?.let { time.coerceIn(0f, it) } ?: time,
                        durationSeconds = duration,
                        isPlaying = state.rate > 0f,
                        status = state.status,
                    )
                }
            }
        }
    }

    private fun observeEvents() {
        viewModelScope.launch {
            player.events.collect(::handlePlayerEvent)
        }
    }

    private fun handlePlayerEvent(event: KitharaPlayerEvent) {
        when (event) {
            is KitharaPlayerEvent.CurrentItemChanged -> onCurrentItemChanged(event.itemId)

            is KitharaPlayerEvent.TrackStatusChanged ->
                onTrackStatusChanged(event.itemId, event.status)

            is KitharaPlayerEvent.QueueEnded -> _uiState.update {
                it.copy(isPlaying = false, errorMessage = "Playlist ended")
            }

            is KitharaPlayerEvent.QueueItemRemoved,
            is KitharaPlayerEvent.CrossfadeSettingsChanged,
            is KitharaPlayerEvent.PlaybackOrderChanged,
            is KitharaPlayerEvent.ActionAtItemEndChanged -> Unit
        }
    }

    private fun onCurrentItemChanged(itemId: String?) {
        // New track -> drop variant data from the previous one so the
        // Settings tab doesn't briefly show stale chips.
        _uiState.update {
            it.copy(
                currentTrackId = itemId,
                discoveredVariants = emptyList(),
                selectedVariantIndex = null,
                currentVariantLabel = null,
                abrIsAuto = true,
                isSeeking = false,
            )
        }
    }

    private fun onTrackStatusChanged(itemId: String, status: TrackStatus) {
        _uiState.update { state ->
            state.copy(
                playlist = state.playlist.map { entry ->
                    if (entry.id == itemId) entry.copy(trackStatus = status) else entry
                },
            )
        }
        if (status !is TrackStatus.Failed) return
        Log.w(TAG, "Track $itemId failed: ${status.reason}")
        if (itemId == _uiState.value.currentTrackId) {
            setLocalError(status.reason)
        }
    }

    private fun handleItemEvent(itemId: String, event: KitharaItemEvent) {
        when (event) {
            is KitharaItemEvent.VariantsDiscovered -> {
                if (itemId != _uiState.value.currentTrackId) return
                val variants = event.variants
                    .sortedBy { it.bandwidthBps }
                    .map { it.index to (it.name ?: "${it.bandwidthBps / 1000}k") }
                _uiState.update { it.copy(discoveredVariants = variants) }
            }

            is KitharaItemEvent.VariantSelected -> {
                if (itemId != _uiState.value.currentTrackId) return
                _uiState.update { it.copy(selectedVariantIndex = event.variant.index) }
            }

            is KitharaItemEvent.VariantApplied -> {
                if (itemId != _uiState.value.currentTrackId) return
                val variant = event.variant
                val label = variant.name ?: "${variant.bandwidthBps / 1000} kbps"
                _uiState.update { it.copy(currentVariantLabel = label) }
            }

            is KitharaItemEvent.DurationChanged -> onDurationChanged(itemId, event.seconds)

            is KitharaItemEvent.Error -> {
                val state = _uiState.value
                if (itemId == state.currentTrackId && state.errorMessage == null) {
                    setLocalError(event.message)
                }
            }
        }
    }

    private fun onDurationChanged(itemId: String, seconds: Double) {
        _uiState.update { state ->
            val playlist = state.playlist.map { entry ->
                if (entry.id == itemId) entry.copy(duration = seconds) else entry
            }
            if (itemId == state.currentTrackId) {
                state.copy(playlist = playlist, durationSeconds = seconds.toFloat())
            } else {
                state.copy(playlist = playlist)
            }
        }
    }

    private fun setLocalError(message: String) {
        _uiState.update {
            it.copy(
                errorMessage = message,
                isSeeking = false,
                status = PlayerStatus.Failed,
            )
        }
    }

    private fun resolveTrackTitle(source: String): String =
        source.substringAfterLast(File.separatorChar).ifBlank { source }
}

private const val TAG = "KitharaExample"
private const val CACHE_DIR_NAME = "kithara-cache"
private const val EQ_BAND_COUNT = 10
private const val DEFAULT_CROSSFADE_SECONDS = 5.0f

private val DEFAULT_TRACK_URLS = listOf(
    "https://stream.silvercomet.top/track.mp3",
    "https://stream.silvercomet.top/hls/master.m3u8",
    "https://stream.silvercomet.top/drm/master.m3u8",
    "https://cdn-edge.zvq.me/track/streamhq?id=27390231",
    "https://cdn-edge.zvq.me/track/streamhq?id=151585912",
    "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
    "https://ecs-stage-slicer-01.zvq.me/hls/track/95038745_1/master.m3u8",
)

private fun KitharaError.reason(): String = message ?: this::class.simpleName.orEmpty()
