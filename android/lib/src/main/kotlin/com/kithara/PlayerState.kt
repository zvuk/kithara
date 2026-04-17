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

/**
 * Queue-level loading lifecycle of a track.
 *
 * Mirrors the Rust `TrackStatus` enum. Tracks progress through
 * `Pending → Loading → Loaded → Consumed`, with `Slow` and `Failed`
 * as transient/terminal branches.
 */
sealed interface TrackStatus {
    data object Pending : TrackStatus
    data object Loading : TrackStatus
    data object Slow : TrackStatus
    data object Loaded : TrackStatus
    data class Failed(val reason: String) : TrackStatus
    data object Consumed : TrackStatus
}

/**
 * One-shot player events delivered via [KitharaPlayer.events].
 *
 * Distinct from [PlayerState], which carries continuously-updated snapshot data.
 */
sealed interface KitharaPlayerEvent {
    /** The current item changed; [itemId] is null when the queue becomes empty. */
    data class CurrentItemChanged(val itemId: String?) : KitharaPlayerEvent

    /** The current item played to its end successfully. */
    data object PlayedToEnd : KitharaPlayerEvent

    /** The loading/playback status of a queued track changed. */
    data class TrackStatusChanged(val itemId: String, val status: TrackStatus) : KitharaPlayerEvent

    /** Queue reached the end (no more tracks to play). */
    data object QueueEnded : KitharaPlayerEvent
}

/**
 * Transition style for a track switch.
 *
 * Mirrors the Apple-idiomatic namespace-struct pattern:
 * - [None] — immediate cut. Matches AVQueuePlayer's user-initiated
 *   selection idiom (tap on a track in a list).
 * - [Crossfade] — use the player's configured crossfade duration.
 *   Typical for Next/Prev buttons and auto-advance at track end.
 * - [CrossfadeWith] — explicit override.
 */
sealed interface Transition {
    /** Immediate cut. */
    data object None : Transition

    /** Use the player's configured crossfade duration. */
    data object Crossfade : Transition

    /** Use an explicit crossfade duration in seconds. */
    data class CrossfadeWith(val seconds: Float) : Transition
}
