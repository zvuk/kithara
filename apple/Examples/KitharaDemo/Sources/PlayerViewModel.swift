import Combine
import Foundation
import Kithara

@MainActor
final class PlayerViewModel: ObservableObject {
    @Published var status: PlayerStatus = .unknown
    @Published var currentTime: TimeInterval = 0
    @Published var duration: TimeInterval?
    @Published var isPlaying = false
    @Published var errorMessage: String?
    @Published var urlText = ""
    @Published var isSeeking = false
    @Published var currentItemId: String?
    @Published var volume: Float = 1.0
    @Published var isMuted = false
    @Published var selectedRate: Float = 1.0

    private let player = KitharaPlayer()
    private var cancellables = Set<AnyCancellable>()

    init() {
        volume = player.volume
        isMuted = player.isMuted

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
                case let .currentItemChanged(itemId):
                    self.currentItemId = itemId
                case let .volumeChanged(vol):
                    self.volume = vol
                case let .muteChanged(muted):
                    self.isMuted = muted
                case .timeControlStatusChanged, .bufferedDurationChanged:
                    break
                }
            }
            .store(in: &cancellables)
    }

    // MARK: - Track info

    var trackName: String {
        guard let itemId = currentItemId else {
            if !urlText.isEmpty {
                return "Press Play to reload"
            }
            return "No Track"
        }
        if let item = player.items.first(where: { $0.id == itemId }) {
            if let url = URL(string: item.url) {
                let name = url.lastPathComponent
                return name.isEmpty ? item.url : name
            }
            return item.url
        }
        // Track was consumed (played to end).
        if let url = URL(string: urlText), !urlText.isEmpty {
            let name = url.lastPathComponent
            return name.isEmpty ? "Press Play to reload" : "\(name) (ended)"
        }
        return "Press Play to reload"
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

    func loadAndPlay() {
        let url = urlText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !url.isEmpty else {
            errorMessage = "Enter a URL"
            return
        }

        // Reset stale state from previous session.
        errorMessage = nil
        currentTime = 0
        duration = nil
        currentItemId = nil

        let item = KitharaPlayerItem(url: url)

        // Subscribe to item errors (network failures, invalid format, etc.).
        item.eventPublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] event in
                if case let .error(message) = event {
                    self?.errorMessage = message
                    self?.status = .failed
                }
            }
            .store(in: &cancellables)

        item.load()

        do {
            player.removeAllItems()
            try player.insert(item)
            player.play()
        } catch {
            self.errorMessage = "\(error)"
            self.status = .failed
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
            // If the queue is empty (track played to end or never loaded), re-load.
            if player.items.isEmpty && !urlText.isEmpty {
                loadAndPlay()
            } else {
                player.play()
            }
        }
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
