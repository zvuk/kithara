package com.kithara

/**
 * Playback readiness state exposed to Android clients.
 */
enum class PlayerStatus {
    /** Player has not reported a stable readiness state yet. */
    Unknown,

    /** Player is ready to start or continue playback. */
    ReadyToPlay,

    /** Player entered a failed state and cannot continue without recovery. */
    Failed,
}

/**
 * Loading state for a single queued item.
 */
enum class ItemStatus {
    /** Item has not reported a stable readiness state yet. */
    Unknown,

    /** Item is ready to start playback. */
    ReadyToPlay,

    /** Item loading or playback preparation failed. */
    Failed,
}

/**
 * Snapshot of player state for Compose, ViewModel, or Flow consumers.
 *
 * @property bufferedDuration Buffered media duration in seconds.
 * @property currentTime Current playback position in seconds.
 * @property duration Total item duration in seconds, if known.
 * @property error Last reported player error, if any.
 * @property items Current queue snapshot.
 * @property rate Current playback rate.
 * @property status Current player readiness status.
 */
data class PlayerState(
    val bufferedDuration: Double = 0.0,
    val currentTime: Double = 0.0,
    val duration: Double? = null,
    val error: KitharaError? = null,
    val items: List<KitharaPlayerItem> = emptyList(),
    val rate: Float = 0f,
    val status: PlayerStatus = PlayerStatus.Unknown,
)

/**
 * Snapshot of item state for Flow consumers.
 *
 * @property bufferedDuration Buffered media duration in seconds.
 * @property duration Total item duration in seconds, if known.
 * @property error Last reported item error, if any.
 * @property status Current item readiness status.
 */
data class ItemState(
    val bufferedDuration: Double = 0.0,
    val duration: Double? = null,
    val error: KitharaError? = null,
    val status: ItemStatus = ItemStatus.Unknown,
)

internal fun playerStatusFromCode(code: Int): PlayerStatus = when (code) {
    1 -> PlayerStatus.ReadyToPlay
    2 -> PlayerStatus.Failed
    else -> PlayerStatus.Unknown
}

internal fun itemStatusFromCode(code: Int): ItemStatus = when (code) {
    1 -> ItemStatus.ReadyToPlay
    2 -> ItemStatus.Failed
    else -> ItemStatus.Unknown
}
