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
        if (uri != null) {
            viewModel.onFilePicked(uri)
        }
    }

    PlayerScreen(
        uiState = uiState,
        onLoadClick = viewModel::loadAndPlay,
        onPickFileClick = { openFileLauncher.launch(arrayOf("audio/*")) },
        onPlayPauseClick = viewModel::playPause,
        onRateClick = viewModel::setRate,
        onSeekChanged = viewModel::onSeekChanged,
        onSeekFinished = viewModel::onSeekFinished,
        onSeekStarted = viewModel::onSeekStarted,
        onUrlChanged = viewModel::onUrlChanged,
    )
}
