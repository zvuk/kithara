package com.kithara.example

import com.kithara.PlayerStatus

internal data class PlayerUiState(
    val currentTimeSeconds: Float = 0f,
    val durationSeconds: Float? = null,
    val errorMessage: String? = null,
    val isPlaying: Boolean = false,
    val isSeeking: Boolean = false,
    val selectedRate: Float = PlayerViewModel.DefaultRate,
    val status: PlayerStatus = PlayerStatus.Unknown,
    val trackTitle: String = "",
    val url: String = "",
)
