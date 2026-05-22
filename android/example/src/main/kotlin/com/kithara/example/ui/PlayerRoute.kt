package com.kithara.example.ui

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.kithara.example.PlayerViewModel

@Composable
internal fun PlayerRoute(viewModel: PlayerViewModel) {
    val uiState by viewModel.uiState.collectAsStateWithLifecycle()

    PlayerScreen(
        uiState = uiState,
        onEvent = { event ->
            when (event) {
                is PlayerScreenEvent.UrlChanged -> viewModel.onUrlChanged(event.url)
                PlayerScreenEvent.AddClick -> viewModel.addTrack()
                PlayerScreenEvent.PlayPauseClick -> viewModel.playPause()
                PlayerScreenEvent.PrevClick -> viewModel.playPrev()
                PlayerScreenEvent.NextClick -> viewModel.playNext()
                is PlayerScreenEvent.TrackClick -> viewModel.selectTrack(event.trackId)
                is PlayerScreenEvent.RateClick -> viewModel.setRate(event.rate)
                PlayerScreenEvent.SeekStarted -> viewModel.onSeekStarted()
                is PlayerScreenEvent.SeekChanged -> viewModel.onSeekChanged(event.value)
                PlayerScreenEvent.SeekFinished -> viewModel.onSeekFinished()
                is PlayerScreenEvent.RemoveTrackClick -> viewModel.removeTrack(event.trackId)
                is PlayerScreenEvent.VolumeChanged -> viewModel.setVolume(event.volume)
                PlayerScreenEvent.MuteClick -> viewModel.toggleMute()
                is PlayerScreenEvent.EqBandChanged -> viewModel.setEqGain(event.bandIndex, event.gain)
                PlayerScreenEvent.EqResetClick -> viewModel.resetEq()
                is PlayerScreenEvent.CrossfadeChanged -> viewModel.setCrossfadeDuration(event.durationSeconds)
                is PlayerScreenEvent.AbrChanged -> viewModel.setAbrMode(event.variantIndex)
            }
        },
    )
}
