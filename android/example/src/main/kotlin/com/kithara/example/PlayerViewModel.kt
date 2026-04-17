package com.kithara.example

import android.app.Application
import android.net.Uri
import android.util.Log
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.kithara.Kithara
import com.kithara.KitharaError
import com.kithara.KitharaPlayer
import com.kithara.KitharaPlayerEvent
import com.kithara.KitharaPlayerItem
import com.kithara.LogLevel
import com.kithara.PlayerStatus
import com.kithara.TrackStatus
import com.kithara.Transition
import java.io.File
import java.io.IOException
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

    init {
        Kithara.initialize(application, logLevel = LogLevel.Debug)
        // Self-managed cache: /data/data/<package>/files/kithara-cache.
        // We use `filesDir` rather than `cacheDir` because kithara runs
        // its own eviction — the OS-managed `cacheDir` can be cleared
        // under storage pressure, which would desync our bookkeeping.
        // Only clears on app uninstall.
        val cacheDir = File(application.filesDir, "kithara-cache").apply { mkdirs() }.absolutePath
        player = KitharaPlayer(
            config = KitharaPlayer.Config(cacheDir = cacheDir),
        ).apply { defaultRate = _uiState.value.selectedRate }
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

    fun onFilePicked(uri: Uri) {
        viewModelScope.launch {
            val file = try {
                copyDocumentToCache(getApplication(), uri)
            } catch (e: IOException) {
                Log.e(TAG, "Failed to copy file: $uri", e)
                setLocalError(e.message ?: e::class.simpleName.orEmpty())
                return@launch
            }

            enqueue(file.path, file.displayName)
        }
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
        player.defaultRate = rate
        if (_uiState.value.isPlaying) player.play()
    }

    fun onSeekStarted() {
        _uiState.update { it.copy(isSeeking = true) }
    }

    fun onSeekChanged(value: Float) {
        _uiState.update { it.copy(currentTimeSeconds = value) }
    }

    fun onSeekFinished() {
        val target = _uiState.value.currentTimeSeconds.toDouble()
        player.seek(target) { ok ->
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

        val entry = PlaylistEntry(id = item.id, name = name, url = url)
        val wasEmpty = _uiState.value.playlist.isEmpty()
        _uiState.update { it.copy(playlist = it.playlist + entry) }

        if (autoPlay && wasEmpty) {
            switchTo(entry.id, Transition.None)
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
                        _uiState.update { it.copy(currentTrackId = event.itemId) }
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
                    is KitharaPlayerEvent.QueueEnded -> {
                        _uiState.update { it.copy(isPlaying = false, errorMessage = "Playlist ended") }
                    }
                    is KitharaPlayerEvent.PlayedToEnd -> Unit
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
