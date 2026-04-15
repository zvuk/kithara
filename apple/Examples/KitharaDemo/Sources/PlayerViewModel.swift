import Combine
import Foundation
import Kithara

struct PlaylistEntry: Identifiable, Equatable {
    let id: UUID
    let name: String
    let url: String
    var duration: TimeInterval?

    init(url: String, name: String? = nil, id: UUID = UUID()) {
        self.id = id
        self.url = url
        self.name = name ?? trackName(for: url)
        self.duration = nil
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
    @Published private(set) var currentTrackId: UUID?
    @Published var volume: Float = 1.0
    @Published var isMuted = false
    @Published var selectedRate: Float = 1.0
    @Published var eqGains: [Float] = []
    @Published var currentVariantLabel: String?
    @Published private(set) var discoveredVariants: [(index: UInt32, label: String)] = []
    @Published var abrIsAuto = true
    @Published var selectedVariantIndex: UInt32?
    @Published var crossfadeDuration: Float = 0

    private let player = KitharaPlayer()
    private var cancellables = Set<AnyCancellable>()
    private var items: [UUID: KitharaPlayerItem] = [:]
    private var itemCancellables: [UUID: AnyCancellable] = [:]
    private var pendingSelectEntryId: UUID?
    /// The last item inserted into the player queue for each playlist entry.
    /// Used to remove the stale slot before appending a fresh one during
    /// `performDeferredSelect`, so the queue doesn't grow unbounded.
    private var lastInsertedItems: [UUID: KitharaPlayerItem] = [:]

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
        configureDrm(on: player)
        volume = player.volume
        isMuted = player.isMuted
        player.defaultRate = selectedRate
        eqGains = Array(repeating: 0, count: player.eqBandCount)
        // Match kithara-app's AppConfig::DEFAULT_CROSSFADE_SECONDS (5.0s).
        player.crossfadeDuration = Self.defaultCrossfadeSeconds
        crossfadeDuration = Self.defaultCrossfadeSeconds

        player.eventPublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] event in
                guard let self else { return }
                switch event {
                case let .timeChanged(seconds):
                    if !self.isSeeking { self.currentTime = seconds }
                case let .rateChanged(rate):
                    self.isPlaying = rate > 0
                case let .statusChanged(ffiStatus):
                    if self.errorMessage == nil {
                        self.status = PlayerStatus(ffi: ffiStatus)
                    }
                case let .durationChanged(seconds):
                    self.duration = seconds
                case let .error(message):
                    self.errorMessage = message
                    self.status = .failed
                case .currentItemChanged:
                    break
                case let .volumeChanged(vol):
                    self.volume = vol
                case let .muteChanged(muted):
                    self.isMuted = muted
                case .itemDidPlayToEnd:
                    // kithara-play emits ItemDidPlayToEnd for ANY track stop,
                    // including fade-out completion during crossfade. Only
                    // auto-advance when the current track has really played
                    // to (near) its end — otherwise a crossfade triggers a
                    // cascade of false "ended" events.
                    if let dur = self.duration, dur > 0, self.currentTime >= dur - 1.0 {
                        self.playNext()
                    }
                case .bufferedDurationChanged:
                    break
                case .timeControlStatusChanged:
                    break
                }
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
        let entry = PlaylistEntry(url: url)

        let item = KitharaPlayerItem(url: url)
        subscribeItem(entryId: entry.id, item: item)
        items[entry.id] = item

        playlist.append(entry)
        item.load()

        do {
            try player.insert(item)
            lastInsertedItems[entry.id] = item
            if autoPlay {
                currentTrackId = entry.id
                status = .unknown
                currentTime = 0
                duration = nil
                currentVariantLabel = nil
                selectedVariantIndex = nil
                abrIsAuto = true
                discoveredVariants = []
                player.play()
            }
            return entry
        } catch {
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
                currentTrackId = first.id
                player.play()
                return
            }
            player.play()
        }
    }

    func playNext() {
        let nextIdx = currentTrackIndex + 1
        guard nextIdx < playlist.count else { return }
        switchTo(index: nextIdx)
    }

    func playPrev() {
        let prevIdx = max(currentTrackIndex - 1, 0)
        guard prevIdx != currentTrackIndex else { return }
        switchTo(index: prevIdx)
    }

    func selectTrack(_ trackId: UUID) {
        guard let idx = playlist.firstIndex(where: { $0.id == trackId }) else { return }
        if idx == currentTrackIndex { return }
        switchTo(index: idx)
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

    private func switchTo(index: Int) {
        guard let entry = playlist[safe: index] else { return }

        currentTime = 0
        duration = entry.duration
        errorMessage = nil
        isSeeking = false
        currentVariantLabel = nil
        selectedVariantIndex = nil
        abrIsAuto = true
        discoveredVariants = []
        currentTrackId = entry.id

        // Always swap in a fresh item. Resources are consumed by playback
        // (`load_current_item` takes them), so re-selecting the same index
        // with a stale item would be a no-op. A fresh item guarantees
        // `LoadTrack + FadeIn` fire and crossfade kicks in.
        itemCancellables[entry.id]?.cancel()
        let fresh = KitharaPlayerItem(url: entry.url)
        subscribeItem(entryId: entry.id, item: fresh)
        items[entry.id] = fresh
        fresh.load()
        pendingSelectEntryId = entry.id
    }

    private func performDeferredSelect(entryId: UUID) {
        guard let item = items[entryId] else {
            pendingSelectEntryId = nil
            return
        }
        pendingSelectEntryId = nil

        // Swift `playlist` and the FFI player queue can diverge — spawn_auto_load
        // drops entries from the player queue when their initial load fails
        // (e.g. offline at startup), so `playlist.firstIndex(...)` is not a
        // valid FFI index. Drop the stale item for this entry (if any),
        // append the fresh one, and select the last slot. That is always a
        // valid queue position and still crossfades against the current leading
        // track because the engine keeps its mixer tracks separately.
        if let prior = lastInsertedItems[entryId] {
            try? player.remove(prior)
        }

        do {
            try player.insert(item)
            lastInsertedItems[entryId] = item
            let lastIdx = Int(player.itemCount) - 1
            guard lastIdx >= 0 else { return }
            try player.selectItem(at: lastIdx, autoplay: true)
            errorMessage = nil
            if status == .failed {
                status = .unknown
            }
        } catch {
            print("[KitharaDemo] insert/select error for \(trackLabel(entryId)): \(error)")
            errorMessage = "\(error)"
            status = .failed
        }
    }

    private func subscribeItem(entryId: UUID, item: KitharaPlayerItem) {
        itemCancellables[entryId] = item.eventPublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] event in
                guard let self else { return }
                switch event {
                case let .durationChanged(seconds):
                    print("[KitharaDemo] \(self.trackLabel(entryId)) durationChanged: \(seconds)s")
                    if let idx = self.playlist.firstIndex(where: { $0.id == entryId }) {
                        self.playlist[idx].duration = seconds
                        if entryId == self.currentTrackId {
                            self.duration = seconds
                        }
                        if self.pendingSelectEntryId == entryId {
                            self.performDeferredSelect(entryId: entryId)
                        }
                    }
                case let .variantsDiscovered(variants):
                    let labels = variants.map { "\($0.bandwidthBps / 1000)k" }.joined(separator: ",")
                    print("[KitharaDemo] \(self.trackLabel(entryId)) variants: [\(labels)]")
                    if entryId == self.currentTrackId {
                        let sorted = variants.sorted { $0.bandwidthBps < $1.bandwidthBps }
                        self.discoveredVariants = sorted.map { v in
                            let label = v.name ?? "\(v.bandwidthBps / 1000)k"
                            return (index: v.index, label: label)
                        }
                    }
                case let .variantSelected(variant):
                    let label = variant.name ?? "\(variant.bandwidthBps / 1000)k"
                    print("[KitharaDemo] \(self.trackLabel(entryId)) variantSelected: \(label)")
                    if entryId == self.currentTrackId {
                        self.selectedVariantIndex = variant.index
                    }
                case let .variantApplied(variant):
                    let label = variant.name ?? "\(variant.bandwidthBps / 1000) kbps"
                    print("[KitharaDemo] \(self.trackLabel(entryId)) variantApplied: \(label)")
                    if entryId == self.currentTrackId {
                        self.currentVariantLabel = label
                    }
                case let .statusChanged(ffiStatus):
                    print("[KitharaDemo] \(self.trackLabel(entryId)) status: \(ffiStatus)")
                case let .error(message):
                    // Log every track error — not only the current one —
                    // so failures during background pre-load are visible.
                    print("[KitharaDemo] \(self.trackLabel(entryId)) ERROR: \(message)")
                    if entryId == self.currentTrackId || entryId == self.pendingSelectEntryId {
                        self.errorMessage = message
                        self.status = .failed
                    }
                default:
                    break
                }
            }
    }

    private func trackLabel(_ entryId: UUID) -> String {
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
