package com.kithara.example

import com.kithara.PlayerStatus
import com.kithara.TrackStatus

internal data class PlaylistEntry(
    /** Matches `KitharaPlayerItem.id` — stable across queue reorder. */
    val id: String,
    val name: String,
    val url: String,
    val trackStatus: TrackStatus? = null,
)

internal data class PlayerUiState(
    val currentTimeSeconds: Float = 0f,
    val currentTrackId: String? = null,
    val durationSeconds: Float? = null,
    val errorMessage: String? = null,
    val isPlaying: Boolean = false,
    val isSeeking: Boolean = false,
    val playlist: List<PlaylistEntry> = emptyList(),
    val selectedRate: Float = DefaultRate,
    val availableRates: List<Float> = AvailableRates,
    val status: PlayerStatus = PlayerStatus.Unknown,
    val url: String = "",
) {

    val currentTrackIndex: Int = playlist.indexOfFirst { it.id == currentTrackId }

    val trackTitle: String
        get() = playlist.getOrNull(currentTrackIndex)?.name ?: "No Track"

    companion object {
        private const val DefaultRate = 1.0f
        private val AvailableRates: List<Float> = listOf(0.5f, 0.75f, 1.0f, 1.2f, 1.5f, 2.0f)
    }
}
