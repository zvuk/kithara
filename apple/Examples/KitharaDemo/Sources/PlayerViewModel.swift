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

    private let player = KitharaPlayer()
    private var currentItem: KitharaPlayerItem?
    private var loadTask: Task<Void, Never>?
    private var cancellables = Set<AnyCancellable>()

    init() {
        player.statusPublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] in self?.status = $0 }
            .store(in: &cancellables)

        player.timePublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] in
                guard let self, !self.isSeeking else { return }
                self.currentTime = $0
            }
            .store(in: &cancellables)

        player.durationPublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] in self?.duration = $0 }
            .store(in: &cancellables)

        player.ratePublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] in self?.isPlaying = $0 > 0 }
            .store(in: &cancellables)

        player.errorPublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] in self?.errorMessage = "\($0)" }
            .store(in: &cancellables)
    }

    func loadAndPlay() {
        let url = urlText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !url.isEmpty else {
            errorMessage = "Enter a URL"
            return
        }

        errorMessage = nil

        // Cancel any previous load to avoid race conditions.
        loadTask?.cancel()
        loadTask = Task {
            do {
                let item = KitharaPlayerItem(url: url)
                try await item.load()
                guard !Task.isCancelled else { return }
                player.removeAllItems()
                try player.insert(item)
                self.currentItem = item
                player.play()
            } catch is CancellationError {
                // Superseded by a newer load — ignore.
            } catch {
                self.errorMessage = "\(error)"
            }
        }
    }

    func togglePlayPause() {
        if isPlaying {
            player.pause()
        } else {
            player.play()
        }
    }

    func seek(to seconds: TimeInterval) {
        currentTime = seconds
        do {
            try player.seek(to: seconds)
        } catch {
            errorMessage = "\(error)"
        }
    }

    func onSeekStarted() {
        isSeeking = true
    }

    func onSeekEnded(_ value: TimeInterval) {
        isSeeking = false
        seek(to: value)
    }

    var formattedTime: String {
        let current = formatTime(currentTime)
        let total = duration.map(formatTime) ?? "--:--"
        return "\(current) / \(total)"
    }
}

private func formatTime(_ seconds: TimeInterval) -> String {
    let mins = Int(seconds) / 60
    let secs = Int(seconds) % 60
    return String(format: "%d:%02d", mins, secs)
}
