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
import java.io.File
import java.io.IOException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.util.UUID

private const val TAG = "KitharaExample"

internal class PlayerViewModel(application: Application) : AndroidViewModel(application) {
    private val _uiState = MutableStateFlow(PlayerUiState())
    val uiState = _uiState.asStateFlow()

    private val player = KitharaPlayer().apply { defaultRate = _uiState.value.selectedRate }
    private var itemJob: Job? = null
    private var shouldReloadCurrentTrack = false

    init {
        Kithara.initialize(application, logLevel = LogLevel.Debug)
        observePlayer()
        observeEvents()
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

        trackToReplay(state, shouldReloadCurrentTrack)?.let { track ->
            loadTrack(track, force = true)
            return
        }

        player.play()
    }

    fun playNext() {
        val state = _uiState.value
        val current = state.currentTrackIndex
        if (current < 0) return
        if (current >= state.playlist.lastIndex) {
            player.pause()
            return
        }
        loadTrack(state.playlist[current + 1])
    }

    fun playPrev() {
        val state = _uiState.value
        val current = state.currentTrackIndex
        if (current < 0) return
        val prev = current.dec().coerceAtLeast(0)
        loadTrack(state.playlist[prev])
    }


    fun selectTrack(trackId: UUID) {
        val state = _uiState.value
        val track = state.playlist.single { it.id == trackId }
        loadTrack(
            track = track,
            force = shouldReloadSelectedTrack(state, trackId, shouldReloadCurrentTrack),
        )
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
        itemJob?.cancel()
        player.pause()
        player.removeAllItems()
    }

    private fun enqueue(url: String, name: String = resolveTrackTitle(url)) {
        val wasEmpty = _uiState.value.playlist.isEmpty()
        val entry = PlaylistEntry(url = url, name = name)

        _uiState.update { it.copy(playlist = it.playlist + entry) }

        if (wasEmpty) {
            loadTrack(entry)
        }
    }

    private fun loadTrack(track: PlaylistEntry, force: Boolean = false) {
        if (!force && _uiState.value.currentTrackId == track.id) return

        val item = KitharaPlayerItem(track.url).also { it.load() }
        observeItem(item)

        player.removeAllItems()
        try {
            player.insert(item)
        } catch (e: KitharaError) {
            Log.e(TAG, "Failed to insert: ${track.url}", e)
            setLocalError(e.message ?: e::class.simpleName.orEmpty())
            return
        }
        player.play()
        shouldReloadCurrentTrack = false

        _uiState.update {
            it.copy(
                currentTimeSeconds = 0f,
                currentTrackId = track.id,
                durationSeconds = null,
                errorMessage = null,
                isSeeking = false,
                status = PlayerStatus.Unknown,
            )
        }
    }

    private fun observeItem(item: KitharaPlayerItem) {
        itemJob?.cancel()
        itemJob = viewModelScope.launch {
            item.state.collectLatest { state ->
                val error = state.error ?: return@collectLatest
                Log.e(TAG, "Item error for source: ${item.url}", error)
                setLocalError(error.message ?: error::class.simpleName.orEmpty())
            }
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
                    is KitharaPlayerEvent.CurrentItemChanged -> Unit
                    is KitharaPlayerEvent.PlayedToEnd -> {
                        shouldReloadCurrentTrack = true
                        playNext()
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

internal fun trackToReplay(
    state: PlayerUiState,
    shouldReloadCurrentTrack: Boolean,
): PlaylistEntry? {
    if (state.isPlaying || !shouldReloadCurrentTrack) {
        return null
    }

    return state.playlist.getOrNull(state.currentTrackIndex)
}

internal fun shouldReloadSelectedTrack(
    state: PlayerUiState,
    trackId: UUID,
    shouldReloadCurrentTrack: Boolean,
): Boolean = shouldReloadCurrentTrack && state.currentTrackId == trackId
