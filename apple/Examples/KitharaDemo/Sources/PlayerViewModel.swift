import Combine
import Foundation
import Kithara

struct PlaylistEntry: Identifiable, Equatable {
    let id: UUID
    let name: String
    let url: String

    init(url: String, name: String? = nil, id: UUID = UUID()) {
        self.id = id
        self.url = url
        self.name = name ?? trackName(for: url)
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

    private let player = KitharaPlayer()
    private var cancellables = Set<AnyCancellable>()
    private var itemCancellable: AnyCancellable?
    private var shouldReloadCurrentTrack = false

    init() {
        volume = player.volume
        isMuted = player.isMuted
        player.defaultRate = selectedRate

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
                    self.playNext(afterPlaybackEnded: true)
                case .timeControlStatusChanged, .bufferedDurationChanged:
                    break
                }
            }
            .store(in: &cancellables)
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

        let entry = PlaylistEntry(url: url)
        let shouldStartPlayback = playlist.isEmpty

        playlist.append(entry)
        urlText = ""
        errorMessage = nil

        if shouldStartPlayback {
            loadTrack(entry, force: true)
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

    // MARK: - Transport

    func togglePlayPause() {
        if isPlaying {
            player.pause()
        } else {
            if let track = currentTrack, shouldReloadCurrentTrack {
                loadTrack(track, force: true)
                return
            }

            if currentTrack == nil, let firstTrack = playlist.first {
                loadTrack(firstTrack, force: true)
                return
            }

            player.play()
        }
    }

    func playNext() {
        playNext(afterPlaybackEnded: false)
    }

    func playPrev() {
        guard let track = playlist[safe: max(currentTrackIndex - 1, 0)] else {
            return
        }
        loadTrack(track)
    }

    func selectTrack(_ trackId: UUID) {
        guard let track = playlist.first(where: { $0.id == trackId }) else {
            return
        }

        let forceReload = shouldReloadCurrentTrack && currentTrackId == trackId
        loadTrack(track, force: forceReload)
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

    private var currentTrack: PlaylistEntry? {
        playlist[safe: currentTrackIndex]
    }

    private func playNext(afterPlaybackEnded: Bool) {
        guard currentTrackIndex >= 0 else {
            return
        }

        let nextIndex = currentTrackIndex + 1
        if let track = playlist[safe: nextIndex] {
            loadTrack(track, force: true)
            return
        }

        if afterPlaybackEnded {
            shouldReloadCurrentTrack = true
            player.pause()
        }
    }

    private func loadTrack(_ track: PlaylistEntry, force: Bool = false) {
        if !force && currentTrackId == track.id {
            return
        }

        currentTime = 0
        duration = nil
        errorMessage = nil
        isSeeking = false
        status = .unknown
        shouldReloadCurrentTrack = false

        let item = KitharaPlayerItem(url: track.url)
        itemCancellable = item.eventPublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] event in
                if case let .error(message) = event {
                    self?.errorMessage = message
                    self?.status = .failed
                }
            }

        item.load()

        do {
            player.removeAllItems()
            try player.insert(item)
            currentTrackId = track.id
            player.play()
        } catch {
            errorMessage = "\(error)"
            status = .failed
        }
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
