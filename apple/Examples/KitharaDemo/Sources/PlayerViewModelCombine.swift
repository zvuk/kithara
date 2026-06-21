import Combine
import Foundation
import Kithara

/// Combine variant of the demo view-model. Subscribes to
/// `KitharaPlayer.eventPublisher` (single unified stream) and routes
/// every `PlayerEvent` through a `switch` in `handlePlayerEvent`.
/// Per-item events ride through `KitharaPlayerItem.eventPublisher`,
/// also a Combine `PassthroughSubject` under the hood.
@MainActor
final class PlayerViewModelCombine: PlayerViewModelBase {
    private var cancellables = Set<AnyCancellable>()
    /// Per-item event subscriptions keyed by `KitharaPlayerItem.audioId`.
    /// Variant discovery + per-item duration flow through here; queue
    /// lifecycle (status/error/current-item) flows through the
    /// player-level `eventPublisher` instead.
    private var itemCancellables: [TrackId: AnyCancellable] = [:]
    /// Discovered variants per track id. `variantsDiscovered` events
    /// can arrive before the item becomes current (pre-buffering
    /// during crossfade), so we persist them and restore on
    /// `currentItemChanged`.
    private var variantsByTrack: [TrackId: [(index: UInt32, label: String)]] = [:]

    override func bindEvents() {
        player.eventPublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] event in
                self?.handlePlayerEvent(event)
            }
            .store(in: &cancellables)
    }

    override func subscribeItem(_ item: KitharaPlayerItem) {
        let entryId = item.audioId
        itemCancellables[entryId] = item.eventPublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] event in
                self?.handleItemEvent(entryId: entryId, event: event)
            }
    }

    override func unsubscribeItem(_ trackId: TrackId) {
        itemCancellables.removeValue(forKey: trackId)
        variantsByTrack.removeValue(forKey: trackId)
    }

    private func handlePlayerEvent(_ event: PlayerEvent) {
        switch event {
        case let .timeChanged(seconds):
            if !isSeeking { currentTime = seconds }
        case let .rateChanged(rate):
            isPlaying = rate > 0
        case let .statusChanged(status):
            if errorMessage == nil {
                self.status = status
            }
        case let .durationChanged(seconds):
            duration = seconds
        case let .error(message):
            errorMessage = message
            status = .failed
        case let .currentItemChanged(itemId):
            currentTrackId = itemId
            resetPerTrackUi(trackId: itemId)
            if let id = itemId {
                discoveredVariants = variantsByTrack[id] ?? []
            }
        case let .volumeChanged(vol):
            volume = vol
        case let .muteChanged(muted):
            isMuted = muted
        case let .trackStatusChanged(itemId, trackStatus):
            handleTrackStatus(itemId: itemId, status: trackStatus)
        case .queueEnded:
            isPlaying = false
            errorMessage = "Playlist ended"
        case .crossfadeStarted, .crossfadeDurationChanged:
            // Queue drives auto-advance + crossfade timing; UI
            // updates on the subsequent `.currentItemChanged`.
            break
        case .timeControlStatusChanged:
            break
        case .itemDidPlayToEnd:
            // Engine does not auto-advance between queue items; drive
            // the queue forward explicitly. Mirrors AVQueuePlayer.
            player.advanceToNextItem()
        case let .itemDidFail(itemId):
            let label = itemId.map(trackLabel) ?? "(unknown)"
            print("[KitharaDemo] item failed mid-stream: \(label)")
            errorMessage = "Track failed: \(label)"
            status = .failed
        @unknown default:
            break
        }
    }

    private func handleTrackStatus(itemId: TrackId, status: TrackStatus) {
        if let idx = playlist.firstIndex(where: { $0.id == itemId }) {
            playlist[idx].trackStatus = status
        }
        switch status {
        case let .failed(reason):
            print("[KitharaDemo] \(trackLabel(itemId)) FAILED: \(reason)")
            if itemId == currentTrackId {
                errorMessage = reason
                self.status = .failed
            }
        default:
            break
        }
    }

    private func handleItemEvent(entryId: TrackId, event: ItemEvent) {
        switch event {
        case let .durationChanged(seconds):
            if let idx = playlist.firstIndex(where: { $0.id == entryId }) {
                playlist[idx].duration = seconds
                if entryId == currentTrackId {
                    duration = seconds
                }
            }
        case let .variantsDiscovered(variants):
            let sorted = variants.sorted { $0.bandwidthBps < $1.bandwidthBps }
            let mapped = sorted.map { v in
                (index: v.index, label: v.name ?? "\(v.bandwidthBps / 1000)k")
            }
            variantsByTrack[entryId] = mapped
            if entryId == currentTrackId {
                discoveredVariants = mapped
            }
        case let .variantSelected(variant):
            if entryId == currentTrackId {
                selectedVariantIndex = variant.index
            }
        case let .variantApplied(variant):
            if entryId == currentTrackId {
                currentVariantLabel = variant.name ?? "\(variant.bandwidthBps / 1000) kbps"
            }
        case let .error(message):
            print("[KitharaDemo] \(trackLabel(entryId)) item error: \(message)")
            if entryId == currentTrackId, errorMessage == nil {
                errorMessage = message
            }
        case .statusChanged, .loadedRangesChanged, .didReachEnd, .didStall:
            break
        case .didFail:
            print("[KitharaDemo] \(trackLabel(entryId)) aborted mid-stream")
            if entryId == currentTrackId, errorMessage == nil {
                errorMessage = "Track failed mid-stream"
            }
        @unknown default:
            break
        }
    }
}
