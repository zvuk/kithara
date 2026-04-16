package com.kithara.example.ui

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.kithara.example.PlayerViewModel

@Composable
internal fun PlayerRoute(viewModel: PlayerViewModel) {
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()
    val openFileLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.OpenDocument(),
    ) { uri ->
        if (uri != null) viewModel.onFilePicked(uri)
    }

    PlayerScreen(
        uiState = uiState,
        onEvent = { event ->
            when (event) {
                is PlayerScreenEvent.UrlChanged -> viewModel.onUrlChanged(event.url)
                PlayerScreenEvent.AddClick -> viewModel.addTrack()
                PlayerScreenEvent.PickFileClick -> openFileLauncher.launch(arrayOf("audio/*"))
                PlayerScreenEvent.PlayPauseClick -> viewModel.playPause()
                PlayerScreenEvent.PrevClick -> viewModel.playPrev()
                PlayerScreenEvent.NextClick -> viewModel.playNext()
                is PlayerScreenEvent.TrackClick -> viewModel.selectTrack(event.trackId)
                is PlayerScreenEvent.RateClick -> viewModel.setRate(event.rate)
                PlayerScreenEvent.SeekStarted -> viewModel.onSeekStarted()
                is PlayerScreenEvent.SeekChanged -> viewModel.onSeekChanged(event.value)
                PlayerScreenEvent.SeekFinished -> viewModel.onSeekFinished()
            }
        },
    )
}
