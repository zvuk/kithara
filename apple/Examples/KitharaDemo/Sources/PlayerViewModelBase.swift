import Foundation
import Kithara

/// Playlist entry — shared between both demo view-model variants.
struct PlaylistEntry: Identifiable, Equatable {
    /// Matches `KitharaPlayerItem.audioId` — stable across queue reorder.
    let id: TrackId
    let name: String
    let url: String
    let subtitle: String?
    var duration: TimeInterval?
    var trackStatus: TrackStatus?

    init(from item: KitharaPlayerItem) {
        self.id = item.audioId
        self.url = item.url.absoluteString
        self.name = trackName(for: item.url.absoluteString)
        self.subtitle = trackSubtitle(for: item.url.absoluteString)
        self.duration = nil
        self.trackStatus = nil
    }

    static func == (lhs: PlaylistEntry, rhs: PlaylistEntry) -> Bool {
        lhs.id == rhs.id
    }
}

/// Base view-model holding the entire UI-facing surface plus all
/// player commands (transport, EQ, ABR, playlist, seek). Event
/// subscriptions live in subclasses so the **same** UI works against
/// either Combine (`KitharaPlayer.eventPublisher`) or RxSwift
/// (`KitharaPlayer.rxRate`, `rxCurrentAudioItem`, …) via `KitharaRx`.
///
/// Subclass contract: override the three `bindEvents()`,
/// `subscribeItem(_:)`, and `unsubscribeItem(_:)` hooks. Everything
/// else is shared.
@MainActor
class PlayerViewModelBase: ObservableObject {
    @Published var status: PlayerStatus = .unknown
    @Published var currentTime: TimeInterval = 0
    @Published var duration: TimeInterval?
    @Published var isPlaying = false
    @Published var errorMessage: String?
    @Published var urlText = ""
    @Published var isSeeking = false
    @Published var playlist: [PlaylistEntry] = []
    @Published var currentTrackId: TrackId?
    @Published var volume: Float = 1.0
    @Published var isMuted = false
    @Published var selectedRate: Float = 1.0
    @Published var eqGains: [Float] = []
    @Published var currentVariantLabel: String?
    @Published var discoveredVariants: [(index: UInt32, label: String)] = []
    @Published var abrIsAuto = true
    @Published var selectedVariantIndex: UInt32?
    @Published var crossfadeDuration: Float = 0
    @Published var shuffleEnabled = false
    @Published var repeatEnabled = false

    /// Engine instance. Subclasses install event subscriptions during
    /// `bindEvents()` (invoked from `init`).
    let player = KitharaPlayer(
        config: KitharaPlayer.Config(
            cacheDir: PlayerViewModelBase.defaultCacheDir
        )
    )

    /// Self-managed cache directory: `~/Library/Application Support/kithara`.
    ///
    /// Uses Application Support instead of Caches because kithara runs
    /// its own eviction; the system-managed `Caches/` dir can be
    /// purged at any time, which would desync our on-disk bookkeeping.
    /// The directory is created on demand and marked as excluded from
    /// iCloud backup — cached media is large and regenerable.
    static var defaultCacheDir: String? {
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

    static let defaultCrossfadeSeconds: Float = 5.0

    /// Bundled playlist mirroring `crates/kithara-app/app.yaml`'s
    /// `playlist.tracks`. Order is load-bearing: XCUITest references
    /// rows by index via `track-<index>` accessibility id.
    static let defaultTrackURLs: [String] = [
        "https://stream.silvercomet.top/track.mp3",
        "https://stream.silvercomet.top/hls/master.m3u8",
        "https://stream.silvercomet.top/drm/master.m3u8",
        "https://cdn-edge.zvq.me/track/streamhq?id=27390231",
        "https://cdn-edge.zvq.me/track/streamhq?id=151585912",
        "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
        "https://cdn-edge.zvq.me/track/streamhq?id=138535169",
        "https://cdn-edge.zvq.me/track/streamhq?id=130432502",
        "https://cdn-edge.zvq.me/track/streamhq?id=132017169",
        "https://ecs-stage-slicer-01.zvq.me/hls/track/176000075_1/master.m3u8",
        "https://ecs-stage-slicer-01.zvq.me/hls/track/176000109_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/173388194_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/180082552_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/5807750_3/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/50984034_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/79829257_2/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/171515249_1/master.m3u8",
        "https://cdn-hls-slicer.zvuk.com/drm/track/59232754_2/master.m3u8",
    ]

    init() {
        volume = player.volume
        isMuted = player.isMuted
        player.playingRate = selectedRate
        eqGains = Array(repeating: 0, count: player.eqBandCount)
        player.crossfadeDuration = Self.defaultCrossfadeSeconds
        crossfadeDuration = Self.defaultCrossfadeSeconds

        for provider in bundledDrmProviders() {
            let salt = provider.salt
            let cipherKey = provider.cipherKey
            let processor = ClosureKeyProcessor { encryptedKey, _ in
                let cipher = Cipher(key: cipherKey + salt)
                return cipher.decrypt(encryptedKey)
            }
            let rule = KitharaPlayer.KeyRule(
                processor: processor,
                domains: provider.domains,
                headers: provider.headers,
                queryParams: nil,
                salt: salt
            )
            player.setupHlsAes(rule: rule)
        }

        bindEvents()

        for url in Self.defaultTrackURLs {
            appendTrack(url: url, autoPlay: false)
        }
    }

    // MARK: - Subclass hooks

    /// Attach subscriptions to player-level event streams. Called
    /// from `init` AFTER DRM setup. Combine subclass attaches a
    /// Combine `sink`; Rx subclass attaches RxSwift `subscribe`.
    func bindEvents() {
        fatalError("override in subclass")
    }

    /// Attach subscriptions to per-item event streams for `item`.
    /// Called from `appendTrack` so per-item state (duration,
    /// variants, errors) arrives even before the item becomes
    /// current. Must be idempotent — `removeTrack` is the dual.
    func subscribeItem(_ item: KitharaPlayerItem) {
        fatalError("override in subclass")
    }

    /// Drop the subscription bag previously installed by
    /// `subscribeItem` for `trackId`. Called from `removeTrack`.
    func unsubscribeItem(_ trackId: TrackId) {
        fatalError("override in subclass")
    }

    // MARK: - Track info

    var trackName: String {
        playlist[safe: currentTrackIndex]?.name ?? "No Track"
    }

    var currentTrackUrl: String? {
        playlist[safe: currentTrackIndex]?.url
    }

    var trackSubtitle: String? {
        playlist[safe: currentTrackIndex]?.subtitle
    }

    var currentTrackIndex: Int {
        playlist.firstIndex { $0.id == currentTrackId } ?? -1
    }

    // MARK: - Time formatting

    var formattedCurrentTime: String { formatTime(currentTime) }
    var formattedDuration: String { duration.map(formatTime) ?? "--:--" }

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
    func appendTrack(url: String, autoPlay: Bool) -> PlaylistEntry? {
        let item = KitharaPlayerItem(url: url)
        subscribeItem(item)

        do {
            try player.append(item)
            let entry = PlaylistEntry(from: item)
            playlist.append(entry)
            if autoPlay {
                try player.selectItem(item)
                player.play()
            }
            return entry
        } catch {
            unsubscribeItem(item.audioId)
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
        player.playingRate = rate
        if isPlaying {
            player.play()
        }
    }

    // MARK: - Stop / Next / Update peak bitrate

    func stop() {
        player.stop()
        playlist.removeAll()
        currentTrackId = nil
        isPlaying = false
        currentTime = 0
        duration = nil
    }

    func advanceToNextItem() {
        player.advanceToNextItem()
    }

    func updatePeakBitrate(wifi: Double, cellular: Double) {
        player.updatePeakBitrate(wifi: wifi, cellular: cellular)
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
                switchTo(entryId: first.id, transition: .none)
                return
            }
            player.play()
        }
    }

    func playNext() {
        let nextIdx = currentTrackIndex + 1
        guard nextIdx < playlist.count else { return }
        switchTo(index: nextIdx, transition: .crossfade)
    }

    func playPrev() {
        let prevIdx = max(currentTrackIndex - 1, 0)
        guard prevIdx != currentTrackIndex else { return }
        switchTo(index: prevIdx, transition: .crossfade)
    }

    func selectTrack(_ trackId: TrackId) {
        guard let idx = playlist.firstIndex(where: { $0.id == trackId }) else { return }
        if idx == currentTrackIndex { return }
        switchTo(index: idx, transition: .none)
    }

    func removeTrack(_ trackId: TrackId) {
        guard let item = player.items().first(where: { $0.audioId == trackId }) else { return }
        do {
            try player.remove(item)
        } catch {
            print("[KitharaDemo] remove failed: \(error)")
        }
        unsubscribeItem(trackId)
        playlist.removeAll { $0.id == trackId }
    }

    // MARK: - Seek

    func onSeekStarted() {
        isSeeking = true
    }

    func onSeekEnded(_ value: TimeInterval) {
        currentTime = value
        player.seek(
            to: value,
            tolerance: nil,
            completionHandler: SeekHandler { [weak self] finished in
                DispatchQueue.main.async {
                    self?.isSeeking = false
                    if !finished {
                        self?.errorMessage = "Seek failed"
                    }
                }
            }
        )
    }

    // MARK: - Helpers used by both subclasses

    func switchTo(index: Int, transition: Transition) {
        guard let entry = playlist[safe: index] else { return }
        switchTo(entryId: entry.id, transition: transition)
    }

    func switchTo(entryId: TrackId, transition: Transition) {
        guard let item = player.items().first(where: { $0.audioId == entryId }) else {
            errorMessage = "item \(entryId) not in queue"
            return
        }
        do {
            try player.selectItem(item, transition: transition)
        } catch {
            print("[KitharaDemo] switch failed for \(trackLabel(entryId)): \(error)")
            errorMessage = "\(error)"
            status = .failed
        }
    }

    func resetPerTrackUi(trackId: TrackId?) {
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

    func trackLabel(_ entryId: TrackId) -> String {
        let name = playlist.first(where: { $0.id == entryId })?.name ?? "unknown"
        return "[\(name)]"
    }
}

// MARK: - SeekCallback implementation

final class SeekHandler: SeekCallback, @unchecked Sendable {
    private let handler: (Bool) -> Void

    init(handler: @escaping (Bool) -> Void) {
        self.handler = handler
    }

    func onComplete(finished: Bool) {
        handler(finished)
    }
}

// MARK: - File-private helpers

private func formatTime(_ seconds: TimeInterval) -> String {
    let mins = Int(seconds) / 60
    let secs = Int(seconds) % 60
    return String(format: "%d:%02d", mins, secs)
}

private func trackSubtitle(for source: String) -> String? {
    guard let url = URL(string: source) else { return nil }
    let components = url.pathComponents.filter { $0 != "/" }
    guard components.count >= 3 else { return nil }
    let album = components[components.count - 2]
    let artist = components[components.count - 3]
    return "\(artist) / \(album)"
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

extension Array {
    subscript(safe index: Int) -> Element? {
        guard indices.contains(index) else {
            return nil
        }
        return self[index]
    }
}
