import Combine
import Foundation
import Kithara

struct PlaylistEntry: Identifiable, Equatable {
    /// Matches `KitharaPlayerItem.id` — stable across queue reorder.
    let id: String
    let name: String
    let url: String
    var duration: TimeInterval?
    var trackStatus: TrackStatus?

    init(from item: KitharaPlayerItem) {
        self.id = item.id
        self.url = item.url
        self.name = trackName(for: item.url)
        self.duration = nil
        self.trackStatus = nil
    }

    static func == (lhs: PlaylistEntry, rhs: PlaylistEntry) -> Bool {
        lhs.id == rhs.id
    }
}

@MainActor
final class PlayerViewModel: ObservableObject {
    @Published var status: PlayerStatus = .unknown
    @Published var currentTime: TimeInterval = 0
    @Published var duration: TimeInterval?
    @Published var isPlaying = false
    @Published var errorMessage: String?
    @Published var urlText = ""
    @Published var isSeeking = false
    @Published private(set) var playlist: [PlaylistEntry] = []
    @Published private(set) var currentTrackId: String?
    @Published var volume: Float = 1.0
    @Published var isMuted = false
    @Published var selectedRate: Float = 1.0
    @Published var eqGains: [Float] = []
    @Published var currentVariantLabel: String?
    @Published private(set) var discoveredVariants: [(index: UInt32, label: String)] = []
    @Published var abrIsAuto = true
    @Published var selectedVariantIndex: UInt32?
    @Published var crossfadeDuration: Float = 0

    private let player = KitharaPlayer(
        config: KitharaPlayer.Config(
            keyRules: makeZvukKeyRules(),
            cacheDir: PlayerViewModel.defaultCacheDir
        )
    )

    /// Self-managed cache directory: `~/Library/Application Support/kithara`.
    ///
    /// Uses Application Support instead of Caches because kithara runs
    /// its own eviction; the system-managed `Caches/` dir can be purged
    /// at any time, which would desync our on-disk bookkeeping. The
    /// directory is created on demand and marked as excluded from iCloud
    /// backup — cached media is large and regenerable.
    private static var defaultCacheDir: String? {
        let fm = FileManager.default
        guard let base = fm
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first?
            .appendingPathComponent("kithara", isDirectory: true)
        else { return nil }

        try? fm.createDirectory(at: base, withIntermediateDirectories: true)

        var url = base
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? url.setResourceValues(values)

        return url.path
    }
    private var cancellables = Set<AnyCancellable>()
    /// Per-item event subscriptions keyed by `KitharaPlayerItem.id`.
    /// Variant discovery and item-level duration flow through here;
    /// queue lifecycle (status/error/current-item) flows through
    /// `player.eventPublisher` instead.
    private var itemCancellables: [String: AnyCancellable] = [:]

    static let defaultCrossfadeSeconds: Float = 5.0

    static let defaultTrackURLs: [String] = [
        "https://stream.silvercomet.top/track.mp3",
        "https://stream.silvercomet.top/hls/master.m3u8",
        "https://stream.silvercomet.top/drm/master.m3u8",
        "https://cdn-edge.zvq.me/track/streamhq?id=27390231",
        "https://cdn-edge.zvq.me/track/streamhq?id=151585912",
        "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
        "https://ecs-stage-slicer-01.zvq.me/hls/track/95038745_1/master.m3u8",
    ]

    init() {
        volume = player.volume
        isMuted = player.isMuted
        player.defaultRate = selectedRate
        eqGains = Array(repeating: 0, count: player.eqBandCount)
        player.crossfadeDuration = Self.defaultCrossfadeSeconds
        crossfadeDuration = Self.defaultCrossfadeSeconds

        player.eventPublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] event in
                self?.handlePlayerEvent(event)
            }
            .store(in: &cancellables)

        for url in Self.defaultTrackURLs {
            appendTrack(url: url, autoPlay: false)
        }
    }

    // MARK: - Track info

    var trackName: String {
        playlist[safe: currentTrackIndex]?.name ?? "No Track"
    }

    var currentTrackIndex: Int {
        playlist.firstIndex { $0.id == currentTrackId } ?? -1
    }

    // MARK: - Time formatting

    var formattedCurrentTime: String {
        formatTime(currentTime)
    }

    var formattedDuration: String {
        duration.map(formatTime) ?? "--:--"
    }

    // MARK: - Volume

    var volumeIcon: String {
        if isMuted || volume == 0 {
            return "speaker.slash.fill"
        } else if volume < 0.5 {
            return "speaker.wave.1.fill"
        } else {
            return "speaker.wave.3.fill"
        }
    }

    func toggleMute() {
        let newValue = !player.isMuted
        player.isMuted = newValue
        isMuted = newValue
    }

    func commitVolume() {
        player.volume = volume
    }

    // MARK: - Load & Play

    func addTrack() {
        let url = urlText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !url.isEmpty else {
            errorMessage = "Enter a URL"
            return
        }
        urlText = ""
        errorMessage = nil
        appendTrack(url: url, autoPlay: playlist.isEmpty)
    }

    @discardableResult
    private func appendTrack(url: String, autoPlay: Bool) -> PlaylistEntry? {
        let item = KitharaPlayerItem(url: url)
        subscribeItem(item)

        do {
            try player.insert(item)
            let entry = PlaylistEntry(from: item)
            playlist.append(entry)
            if autoPlay {
                try player.selectItem(item)
                player.play()
            }
            return entry
        } catch {
            itemCancellables.removeValue(forKey: item.id)
            print("[KitharaDemo] insert error: \(error)")
            errorMessage = "\(error)"
            status = .failed
            return nil
        }
    }

    // MARK: - EQ

    func setEqGain(band: Int, db: Float) {
        player.setEqGain(band: band, gainDb: db)
        if band < eqGains.count {
            eqGains[band] = db
        }
    }

    func resetEq() {
        player.resetEq()
        eqGains = Array(repeating: 0, count: eqGains.count)
    }

    // MARK: - ABR

    func setAbrMode(variantIndex: UInt32?) {
        if let idx = variantIndex {
            player.setAbrMode(.manual(variantIndex: Int(idx)))
            abrIsAuto = false
            selectedVariantIndex = idx
        } else {
            player.setAbrMode(.auto)
            abrIsAuto = true
            selectedVariantIndex = nil
        }
    }

    // MARK: - Rate

    static let availableRates: [Float] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0]

    func setRate(_ rate: Float) {
        selectedRate = rate
        player.defaultRate = rate
        if isPlaying {
            player.play()
        }
    }

    // MARK: - Crossfade

    static let crossfadeRange: ClosedRange<Float> = 0...8

    func setCrossfadeDuration(_ seconds: Float) {
        let clamped = min(max(seconds, Self.crossfadeRange.lowerBound), Self.crossfadeRange.upperBound)
        crossfadeDuration = clamped
        player.crossfadeDuration = clamped
    }

    // MARK: - Transport

    func togglePlayPause() {
        if isPlaying {
            player.pause()
        } else {
            if currentTrackId == nil, let first = playlist.first {
                // First-time play on empty session → immediate cut.
                switchTo(entryId: first.id, transition: .none)
                return
            }
            player.play()
        }
    }

    func playNext() {
        let nextIdx = currentTrackIndex + 1
        guard nextIdx < playlist.count else { return }
        // Next button → crossfade, symmetric with auto-advance at
        // track end.
        switchTo(index: nextIdx, transition: .crossfade)
    }

    func playPrev() {
        let prevIdx = max(currentTrackIndex - 1, 0)
        guard prevIdx != currentTrackIndex else { return }
        // Prev button → crossfade, symmetric with Next.
        switchTo(index: prevIdx, transition: .crossfade)
    }

    func selectTrack(_ trackId: String) {
        guard let idx = playlist.firstIndex(where: { $0.id == trackId }) else { return }
        if idx == currentTrackIndex { return }
        // Tap on a track in the list → immediate cut (AVQueuePlayer idiom).
        switchTo(index: idx, transition: .none)
    }

    func removeTrack(_ trackId: String) {
        guard let item = player.items.first(where: { $0.id == trackId }) else { return }
        do {
            try player.remove(item)
        } catch {
            print("[KitharaDemo] remove failed: \(error)")
        }
        itemCancellables.removeValue(forKey: trackId)
        playlist.removeAll { $0.id == trackId }
    }

    // MARK: - Seek

    func onSeekStarted() {
        isSeeking = true
    }

    func onSeekEnded(_ value: TimeInterval) {
        currentTime = value
        player.seek(to: value, callback: SeekHandler { [weak self] finished in
            DispatchQueue.main.async {
                self?.isSeeking = false
                if !finished {
                    self?.errorMessage = "Seek failed"
                }
            }
        })
    }

    // MARK: - Private

    private func switchTo(index: Int, transition: Transition) {
        guard let entry = playlist[safe: index] else { return }
        switchTo(entryId: entry.id, transition: transition)
    }

    private func switchTo(entryId: String, transition: Transition) {
        guard let item = player.items.first(where: { $0.id == entryId }) else {
            errorMessage = "item \(entryId) not in queue"
            return
        }
        do {
            try player.selectItem(item, transition: transition)
            // Per-track UI reset happens on `.currentItemChanged` once the
            // Queue actually switches (matches crossfade timing).
        } catch {
            print("[KitharaDemo] switch failed for \(trackLabel(entryId)): \(error)")
            errorMessage = "\(error)"
            status = .failed
        }
    }

    private func handlePlayerEvent(_ event: PlayerEvent) {
        switch event {
        case let .timeChanged(seconds):
            if !isSeeking { currentTime = seconds }
        case let .rateChanged(rate):
            isPlaying = rate > 0
        case let .statusChanged(ffiStatus):
            if errorMessage == nil {
                status = PlayerStatus(ffi: ffiStatus)
            }
        case let .durationChanged(seconds):
            duration = seconds
        case let .error(message):
            errorMessage = message
            status = .failed
        case let .currentItemChanged(itemId):
            currentTrackId = itemId
            resetPerTrackUi(trackId: itemId)
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
            // Queue drives auto-advance + crossfade timing; UI updates on
            // the subsequent `.currentItemChanged`.
            break
        case .itemDidPlayToEnd, .bufferedDurationChanged, .timeControlStatusChanged:
            break
        @unknown default:
            break
        }
    }

    private func resetPerTrackUi(trackId: String?) {
        // Don't reset `currentTime`, `status`, or `isPlaying` — the
        // engine drives those via explicit events. Touching them here
        // races with the engine's own updates and causes UI flicker
        // (slider snap to 0 on pause/resume, "Not Ready" blink between
        // tracks). Queue-owned values are the source of truth.
        errorMessage = nil
        isSeeking = false
        currentVariantLabel = nil
        selectedVariantIndex = nil
        abrIsAuto = true
        discoveredVariants = []
        duration = trackId.flatMap { id in
            playlist.first(where: { $0.id == id })?.duration
        }
    }

    private func handleTrackStatus(itemId: String, status: TrackStatus) {
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

    private func subscribeItem(_ item: KitharaPlayerItem) {
        let entryId = item.id
        itemCancellables[entryId] = item.eventPublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] event in
                self?.handleItemEvent(entryId: entryId, event: event)
            }
    }

    private func handleItemEvent(entryId: String, event: ItemEvent) {
        switch event {
        case let .durationChanged(seconds):
            if let idx = playlist.firstIndex(where: { $0.id == entryId }) {
                playlist[idx].duration = seconds
                if entryId == currentTrackId {
                    duration = seconds
                }
            }
        case let .variantsDiscovered(variants):
            guard entryId == currentTrackId else { return }
            let sorted = variants.sorted { $0.bandwidthBps < $1.bandwidthBps }
            discoveredVariants = sorted.map { v in
                let label = v.name ?? "\(v.bandwidthBps / 1000)k"
                return (index: v.index, label: label)
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
            // Surface only the first error per track — the pipeline can
            // retry internally and emit the same error many times.
            if entryId == currentTrackId, errorMessage == nil {
                errorMessage = message
            }
        case .statusChanged, .bufferedDurationChanged:
            break
        @unknown default:
            break
        }
    }

    private func trackLabel(_ entryId: String) -> String {
        let name = playlist.first(where: { $0.id == entryId })?.name ?? "unknown"
        return "[\(name)]"
    }
}

// MARK: - SeekCallback implementation

private final class SeekHandler: SeekCallback, @unchecked Sendable {
    private let handler: (Bool) -> Void

    init(handler: @escaping (Bool) -> Void) {
        self.handler = handler
    }

    func onComplete(finished: Bool) {
        handler(finished)
    }
}

// MARK: - Helpers

private func formatTime(_ seconds: TimeInterval) -> String {
    let mins = Int(seconds) / 60
    let secs = Int(seconds) % 60
    return String(format: "%d:%02d", mins, secs)
}

private func trackName(for source: String) -> String {
    if let url = URL(string: source) {
        let name = url.lastPathComponent
        if !name.isEmpty {
            return name
        }
    }

    let name = source.split(separator: "/").last.map(String.init) ?? source
    return name.isEmpty ? source : name
}

private extension Array {
    subscript(safe index: Int) -> Element? {
        guard indices.contains(index) else {
            return nil
        }
        return self[index]
    }
}
