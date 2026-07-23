package com.kithara.example

import android.app.Application
import android.util.Log
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.kithara.AssetStore
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
import java.io.File
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

private const val TAG = "KitharaExample"

internal class PlayerViewModel(application: Application) : AndroidViewModel(application) {
    private val _uiState = MutableStateFlow(PlayerUiState())
    val uiState = _uiState.asStateFlow()

    private val player: KitharaPlayer

    /**
     * Per-item event subscriptions keyed by [KitharaPlayerItem.id]. Variant
     * discovery flows through here; queue-level events flow through
     * [observeEvents]. Mirrors the iOS demo `itemCancellables` map so
     * variant and duration data show up identically on both platforms.
     */
    private val itemSubscriptions: MutableMap<String, Job> = mutableMapOf()

    init {
        Kithara.initialize(application, logLevel = LogLevel.Debug)
        // Self-managed cache: /data/data/<package>/files/kithara-cache.
        // We use `filesDir` rather than `cacheDir` because kithara runs
        // its own eviction — the OS-managed `cacheDir` can be cleared
        // under storage pressure, which would desync our bookkeeping.
        // Only clears on app uninstall.
        val cacheDir = File(application.filesDir, "kithara-cache").apply { mkdirs() }.absolutePath
        val store = AssetStore(root = cacheDir)
        player = KitharaPlayer(
            config = KitharaPlayer.Config(store = store),
        ).apply { playingRate = _uiState.value.selectedRate }
        // Register a wildcard `"*"` HLS-AES decryptor — the closure
        // derives the cipher per-call from the player-supplied salt
        // (`X-Encrypted-Key` header, generated and attached by the
        // player on every outgoing request).
        val cipherKey = readZvukCipherKey(application)
        player.setupHlsAes { encryptedKey, salt ->
            kitharaCipherDecrypt(cipherKey + salt, encryptedKey)
        }
        readZvukAuthToken(application)?.let(player::setupNetwork)

        // Force-align Android default with iOS (PlayerViewModel.swift:88) — the
        // native player initializes to 1.0s but the demo expects 5.0s on a fresh
        // install, so the Settings tab shows the same value across platforms.
        player.crossfadeDuration = 5.0f
        _uiState.update {
            it.copy(
                volume = player.volume,
                isMuted = player.isMuted,
                crossfadeDuration = player.crossfadeDuration,
                eqGains = List(10) { i -> player.getEqGain(i) }
            )
        }

        observePlayer()
        observeEvents()
        for (url in DEFAULT_TRACK_URLS) {
            enqueue(url, autoPlay = false)
        }
    }

    companion object {
        private val DEFAULT_TRACK_URLS = listOf(
            "https://stream.silvercomet.top/track.mp3",
            "https://stream.silvercomet.top/hls/master.m3u8",
            "https://stream.silvercomet.top/drm/master.m3u8",
            "https://cdn-edge.zvq.me/track/streamhq?id=27390231",
            "https://cdn-edge.zvq.me/track/streamhq?id=151585912",
            "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
            "https://ecs-stage-slicer-01.zvq.me/hls/track/95038745_1/master.m3u8",
        )
    }

    fun onUrlChanged(value: String) {
        _uiState.update { it.copy(url = value, errorMessage = null) }
    }

    fun addTrack() {
        val url = _uiState.value.url.trim()
        if (url.isEmpty()) {
            setLocalError("Enter an audio URL or pick a file")
            return
        }

        _uiState.update { it.copy(url = "", errorMessage = null) }
        enqueue(url)
    }

    fun playPause() {
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

    fun playNext() {
        val state = _uiState.value
        val current = state.currentTrackIndex
        if (current < 0 || current >= state.playlist.lastIndex) return
        // Next button → crossfade, symmetric with auto-advance at track end.
        switchTo(state.playlist[current + 1].id, Transition.Crossfade)
    }

    fun playPrev() {
        val state = _uiState.value
        val current = state.currentTrackIndex
        if (current <= 0) return
        // Prev button → crossfade, symmetric with Next.
        switchTo(state.playlist[current - 1].id, Transition.Crossfade)
    }

    fun selectTrack(trackId: String) {
        // Tap on a track in the list → immediate cut (AVQueuePlayer idiom).
        switchTo(trackId, Transition.None)
    }

    fun setRate(rate: Float) {
        _uiState.update { it.copy(selectedRate = rate) }
        player.playingRate = rate
        if (_uiState.value.isPlaying) player.play()
    }

    fun setVolume(volume: Float) {
        player.volume = volume
        _uiState.update { it.copy(volume = volume) }
    }

    fun toggleMute() {
        val newMuted = !player.isMuted
        player.isMuted = newMuted
        _uiState.update { it.copy(isMuted = newMuted) }
    }

    fun setEqGain(band: Int, gainDb: Float) {
        player.setEqGain(band, gainDb)
        _uiState.update { state ->
            val updated = state.eqGains.toMutableList()
            if (band in updated.indices) {
                updated[band] = gainDb
            }
            state.copy(eqGains = updated)
        }
    }

    fun resetEq() {
        player.resetEq()
        _uiState.update { it.copy(eqGains = List(10) { 0f }) }
    }

    fun setCrossfadeDuration(duration: Float) {
        player.crossfadeDuration = duration
        _uiState.update { it.copy(crossfadeDuration = duration) }
    }

    fun setAbrMode(variantIndex: UInt?) {
        val mode = if (variantIndex != null) {
            com.kithara.ffi.FfiAbrMode.Manual(variantIndex)
        } else {
            com.kithara.ffi.FfiAbrMode.Auto
        }
        player.setAbrMode(mode)
        _uiState.update {
            it.copy(
                abrIsAuto = variantIndex == null,
                selectedVariantIndex = variantIndex,
            )
        }
    }

    fun removeTrack(trackId: String) {
        val item = player.items.firstOrNull { it.id == trackId }
        if (item != null) {
            player.remove(item)
        }
        itemSubscriptions.remove(trackId)?.cancel()
        _uiState.update { state ->
            state.copy(playlist = state.playlist.filterNot { it.id == trackId })
        }
    }

    fun stop() {
        player.stop()
        _uiState.update {
            it.copy(
                playlist = emptyList(),
                currentTrackId = null,
                isPlaying = false,
                currentTimeSeconds = 0f,
                durationSeconds = null,
            )
        }
    }

    fun advanceToNextItem() {
        player.advanceToNextItem()
    }

    fun updatePeakBitrate(wifi: Double, cellular: Double) {
        player.updatePeakBitrate(wifi = wifi, cellular = cellular)
    }

    fun onSeekStarted() {
        _uiState.update { it.copy(isSeeking = true) }
    }

    fun onSeekChanged(value: Float) {
        _uiState.update { it.copy(currentTimeSeconds = value) }
    }

    fun onSeekFinished() {
        val target = _uiState.value.currentTimeSeconds.toDouble()
        player.seek(target, tolerance = null) { ok ->
            if (ok) {
                _uiState.update { it.copy(isSeeking = false) }
            } else {
                Log.e(TAG, "Seek failed")
                _uiState.update {
                    it.copy(
                        errorMessage = "Seek failed",
                        isSeeking = false,
                    )
                }
            }
        }
    }

    override fun onCleared() {
        super.onCleared()
        itemSubscriptions.values.forEach { it.cancel() }
        itemSubscriptions.clear()
        player.pause()
        player.removeAllItems()
    }

    private fun enqueue(
        url: String,
        name: String = resolveTrackTitle(url),
        autoPlay: Boolean = true,
    ) {
        val item = KitharaPlayerItem(url)
        try {
            player.insert(item)
        } catch (e: KitharaError) {
            Log.e(TAG, "Failed to insert: $url", e)
            setLocalError(e.message ?: e::class.simpleName.orEmpty())
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

    private fun handleItemEvent(itemId: String, event: KitharaItemEvent) {
        when (event) {
            is KitharaItemEvent.VariantsDiscovered -> {
                if (itemId != _uiState.value.currentTrackId) return
                val sorted = event.variants.sortedBy { it.bandwidthBps }
                val mapped = sorted.map { variant ->
                    val label = variant.name ?: "${variant.bandwidthBps / 1000}k"
                    variant.index to label
                }
                _uiState.update { it.copy(discoveredVariants = mapped) }
            }
            is KitharaItemEvent.VariantSelected -> {
                if (itemId != _uiState.value.currentTrackId) return
                _uiState.update { it.copy(selectedVariantIndex = event.variant.index) }
            }
            is KitharaItemEvent.VariantApplied -> {
                if (itemId != _uiState.value.currentTrackId) return
                val label = event.variant.name ?: "${event.variant.bandwidthBps / 1000} kbps"
                _uiState.update { it.copy(currentVariantLabel = label) }
            }
            is KitharaItemEvent.DurationChanged -> {
                val seconds = event.seconds
                _uiState.update { state ->
                    val updatedPlaylist = state.playlist.map { entry ->
                        if (entry.id == itemId) entry.copy(duration = seconds) else entry
                    }
                    if (itemId == state.currentTrackId) {
                        state.copy(
                            playlist = updatedPlaylist,
                            durationSeconds = seconds.toFloat(),
                        )
                    } else {
                        state.copy(playlist = updatedPlaylist)
                    }
                }
            }
            is KitharaItemEvent.Error -> {
                if (itemId == _uiState.value.currentTrackId &&
                    _uiState.value.errorMessage == null
                ) {
                    setLocalError(event.message)
                }
            }
        }
    }

    private fun switchTo(trackId: String, transition: Transition) {
        val item = player.items.firstOrNull { it.id == trackId }
        if (item == null) {
            setLocalError("item $trackId not in queue")
            return
        }
        try {
            player.selectItem(item, transition)
            player.play()
            // Don't reset `status` — the engine only emits StatusChanged on
            // real transitions, so touching the field here makes the header
            // flicker to "Not Ready" between tracks.
            _uiState.update {
                it.copy(
                    currentTrackId = trackId,
                    currentTimeSeconds = 0f,
                    durationSeconds = null,
                    errorMessage = null,
                    isSeeking = false,
                )
            }
        } catch (e: KitharaError) {
            Log.e(TAG, "Failed to select: $trackId", e)
            setLocalError(e.message ?: e::class.simpleName.orEmpty())
        }
    }

    private fun observePlayer() {
        viewModelScope.launch {
            player.state.collectLatest { state ->
                _uiState.update { current ->
                    val time = if (current.isSeeking) current.currentTimeSeconds
                    else state.currentTime.toFloat()
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
            player.events.collect { event ->
                when (event) {
                    is KitharaPlayerEvent.CurrentItemChanged -> {
                        // New track → drop variant data from the previous track so
                        // the Settings tab doesn't briefly show stale chips.
                        _uiState.update {
                            it.copy(
                                currentTrackId = event.itemId,
                                discoveredVariants = emptyList(),
                                selectedVariantIndex = null,
                                currentVariantLabel = null,
                                abrIsAuto = true,
                                isSeeking = false,
                            )
                        }
                    }
                    is KitharaPlayerEvent.TrackStatusChanged -> {
                        _uiState.update { state ->
                            state.copy(
                                playlist = state.playlist.map { entry ->
                                    if (entry.id == event.itemId) entry.copy(trackStatus = event.status)
                                    else entry
                                }
                            )
                        }
                        if (event.status is TrackStatus.Failed) {
                            val reason = (event.status as TrackStatus.Failed).reason
                            Log.w(TAG, "Track ${event.itemId} failed: $reason")
                            if (event.itemId == _uiState.value.currentTrackId) {
                                setLocalError(reason)
                            }
                        }
                    }
                    is KitharaPlayerEvent.QueueItemRemoved -> Unit
                    is KitharaPlayerEvent.QueueEnded -> {
                        _uiState.update { it.copy(isPlaying = false, errorMessage = "Playlist ended") }
                    }
                }
            }
        }
    }

    private fun setLocalError(message: String) {
        _uiState.update {
            it.copy(
                errorMessage = message,
                isSeeking = false,
                status = PlayerStatus.Failed
            )
        }
    }

    private fun resolveTrackTitle(source: String): String {
        return source.substringAfterLast(File.separatorChar).ifBlank { source }
    }
}
