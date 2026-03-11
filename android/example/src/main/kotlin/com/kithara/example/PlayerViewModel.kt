package com.kithara.example

import android.app.Application
import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import android.util.Log
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.kithara.Kithara
import com.kithara.KitharaError
import com.kithara.KitharaPlayer
import com.kithara.KitharaPlayerItem
import com.kithara.PlayerStatus
import java.io.File
import java.io.IOException
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import androidx.core.net.toUri

private const val TAG = "KitharaExample"
private const val DefaultTrackTitle = "No Track"

internal class PlayerViewModel(application: Application) : AndroidViewModel(application) {
    private val _uiState = MutableStateFlow(PlayerUiState(trackTitle = DefaultTrackTitle))
    val uiState = _uiState.asStateFlow()

    private var localError: String? = null
    private var player: KitharaPlayer
    private var playerJob: Job? = null
    private var itemJob: Job? = null
    private var sourceUrl: String? = null

    init {
        Kithara.initialize(application)
        player = KitharaPlayer().apply { defaultRate = _uiState.value.selectedRate }
        observePlayer()
    }

    fun onUrlChanged(value: String) {
        localError = null
        _uiState.update { state -> state.copy(errorMessage = null, url = value) }
    }

    fun loadAndPlay() {
        val trimmed = _uiState.value.url.trim()
        if (trimmed.isEmpty()) {
            setLocalError("Enter an audio URL or pick a file")
            return
        }

        viewModelScope.launch(Dispatchers.Default) {
            try {
                prepareAndPlay(trimmed)
            } catch (error: KitharaError) {
                Log.e(TAG, "Load/play failed for source: $trimmed", error)
                setPlaybackError(error)
            }
        }
    }

    fun onFilePicked(uri: Uri) {
        localError = null
        viewModelScope.launch {
            val localPath = try {
                copyDocumentToCache(getApplication(), uri)
            } catch (error: IOException) {
                Log.e(TAG, "Failed to copy file: $uri", error)
                setLocalError(error.message ?: error::class.simpleName.orEmpty())
                return@launch
            }
            _uiState.update { state -> state.copy(url = localPath) }
            withContext(Dispatchers.Default) {
                try {
                    prepareAndPlay(localPath)
                } catch (error: KitharaError) {
                    Log.e(TAG, "Load/play failed for selected file: $uri", error)
                    setPlaybackError(error)
                }
            }
        }
    }


    fun playPause() {
        if (_uiState.value.isPlaying) {
            player.pause()
            return
        }

        if (player.items.isEmpty() && _uiState.value.url.isNotBlank()) {
            loadAndPlay()
            return
        }

        player.play()
    }

    fun setRate(rate: Float) {
        _uiState.update { state -> state.copy(selectedRate = rate) }
        player.defaultRate = rate
        if (_uiState.value.isPlaying) {
            player.play()
        }
    }

    fun onSeekStarted() {
        _uiState.update { state -> state.copy(isSeeking = true) }
    }

    fun onSeekChanged(value: Float) {
        _uiState.update { state -> state.copy(currentTimeSeconds = value) }
    }

    fun onSeekFinished() {
        val target = _uiState.value.currentTimeSeconds.toDouble()
        player.seek(target) { finished ->
            if (finished) {
                _uiState.update { state -> state.copy(isSeeking = false) }
            } else {
                Log.e(TAG, "Seek failed for source: $sourceUrl")
                setLocalError("Seek failed")
            }
        }
    }

    override fun onCleared() {
        super.onCleared()
        itemJob?.cancel()
        playerJob?.cancel()
        player.pause()
        player.removeAllItems()
    }

    private fun observeItem(item: KitharaPlayerItem) {
        itemJob?.cancel()
        itemJob = viewModelScope.launch {
            item.state.collectLatest { state ->
                val error = state.error ?: return@collectLatest
                Log.e(TAG, "Item error for source: $sourceUrl — ${error.message}")
                setPlaybackError(error)
            }
        }
    }

    private fun observePlayer() {
        playerJob?.cancel()
        playerJob = viewModelScope.launch {
            player.state.collectLatest { state ->
                _uiState.update { current ->
                    val playerTime = if (current.isSeeking) {
                        current.currentTimeSeconds
                    } else {
                        state.currentTime.toFloat()
                    }
                    val duration = state.duration?.toFloat()
                    current.copy(
                        currentTimeSeconds = duration?.let { playerTime.coerceIn(0f, it) }
                            ?: playerTime,
                        durationSeconds = duration,
                        errorMessage = localError ?: state.error?.message,
                        isPlaying = state.rate > 0f,
                        status = state.status,
                        trackTitle = sourceUrl?.let(::resolveTrackTitle) ?: DefaultTrackTitle,
                    )
                }
            }
        }
    }

    private fun prepareAndPlay(source: String) {
        resetPlayer()
        localError = null
        sourceUrl = source
        _uiState.update { state ->
            state.copy(
                currentTimeSeconds = 0f,
                durationSeconds = null,
                errorMessage = null,
                isPlaying = false,
                isSeeking = false,
                status = PlayerStatus.Unknown,
                trackTitle = resolveTrackTitle(source),
                url = source,
            )
        }

        val item = KitharaPlayerItem(source)
        observeItem(item)
        item.load()
        player.removeAllItems()
        player.insert(item)
        player.play()
    }

    private fun resetPlayer() {
        itemJob?.cancel()
        player.pause()
        player.removeAllItems()
    }

    private fun setLocalError(message: String) {
        localError = message
        _uiState.update { state ->
            state.copy(
                errorMessage = message,
                isSeeking = false,
                status = PlayerStatus.Failed,
            )
        }
    }

    private fun setPlaybackError(error: KitharaError) {
        setLocalError(error.message ?: error::class.simpleName.orEmpty())
    }

    private fun resolveTrackTitle(source: String): String {
        val parsed = source.toUri().lastPathSegment?.substringAfterLast('/')
        val fallback = source.substringAfterLast('/').substringAfterLast(File.separatorChar)
        return (parsed ?: fallback).ifBlank { source }
    }

    // "content://" URIs can only be read via ContentResolver; Kithara needs a file path.
    private suspend fun copyDocumentToCache(context: Context, uri: Uri): String =
        withContext(Dispatchers.IO) {
            val fileName = queryDisplayName(context, uri)
                ?.takeIf(String::isNotBlank)
                ?: uri.lastPathSegment?.substringAfterLast('/')
                ?: "selected-audio"
            val importDir = File(context.cacheDir, "imports")
            if (!importDir.exists() && !importDir.mkdirs()) {
                throw IOException("Unable to create import directory")
            }
            val outputFile = File(importDir, "${UUID.randomUUID()}-$fileName")
            context.contentResolver.openInputStream(uri)?.use { input ->
                outputFile.outputStream().use { output -> input.copyTo(output) }
            } ?: throw IOException("Unable to open selected file")
            outputFile.absolutePath
        }

    private fun queryDisplayName(context: Context, uri: Uri): String? =
        context.contentResolver.query(
            uri,
            arrayOf(OpenableColumns.DISPLAY_NAME),
            null,
            null,
            null,
        )?.use { cursor ->
            val nameColumn = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
            if (nameColumn < 0 || !cursor.moveToFirst()) return@use null
            cursor.getString(nameColumn)
        }

    internal companion object {
        const val DefaultRate = 1.0f
        val AvailableRates: List<Float> = listOf(0.5f, 0.75f, 1.0f, 1.2f, 1.5f, 2.0f)
    }
}
