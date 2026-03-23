import Combine
import Kithara
import PlaygroundSupport
import SwiftUI

final class SeekHandler: SeekCallback, @unchecked Sendable {
    private let onDone: @Sendable (Bool) -> Void

    init(onDone: @escaping @Sendable (Bool) -> Void) {
        self.onDone = onDone
    }

    func onComplete(finished: Bool) {
        onDone(finished)
    }
}

@MainActor
final class PlaygroundModel: ObservableObject {
    @Published var url = "https://audio-ssl.itunes.apple.com/itunes-assets/AudioPreview116/v4/42/1e/0d/421e0d16-0bc4-f4d6-7ad9-9b16810639bc/mzaf_16772981048115664645.plus.aac.p.m4a"
    @Published var status = "unknown"
    @Published var currentTime: Double = 0
    @Published var duration: Double = 1
    @Published var volume: Float = 1.0
    @Published var isMuted = false
    @Published var rate: Float = 1.0
    @Published var log = "Ready"

    private let player = KitharaPlayer()
    private var cancellables = Set<AnyCancellable>()

    init() {
        player.eventPublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] event in
                guard let self else { return }

                switch event {
                case let .timeChanged(seconds):
                    self.currentTime = seconds
                case let .durationChanged(seconds):
                    self.duration = max(seconds, 1)
                case let .statusChanged(value):
                    self.status = "\(PlayerStatus(ffi: value))"
                case let .rateChanged(value):
                    self.rate = value
                case let .error(message):
                    self.log = "Player error: \(message)"
                default:
                    break
                }
            }
            .store(in: &cancellables)
    }

    func load() {
        let item = KitharaPlayerItem(url: url)
        item.eventPublisher
            .receive(on: DispatchQueue.main)
            .sink { [weak self] event in
                guard case let .error(message) = event else { return }
                self?.log = "Item error: \(message)"
            }
            .store(in: &cancellables)

        item.load()

        do {
            player.removeAllItems()
            try player.insert(item)
            log = "Loaded: \(item.id)"
            currentTime = 0
        } catch {
            log = "Insert failed: \(error)"
        }
    }

    func play() {
        player.play()
        log = "Play"
    }

    func pause() {
        player.pause()
        log = "Pause"
    }

    func seek() {
        let target = currentTime
        player.seek(to: target, callback: SeekHandler { [weak self] done in
            Task { @MainActor in
                self?.log = done ? "Seek done: \(Int(target))s" : "Seek failed"
            }
        })
    }

    func commitVolume() {
        player.volume = volume
        log = "Volume: \(Int(volume * 100))%"
    }

    func setMuted() {
        player.isMuted = isMuted
    }

    func setRate(_ value: Float) {
        player.defaultRate = value
        player.play()
        rate = value
    }
}

struct PlaygroundView: View {
    @StateObject private var model = PlaygroundModel()

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Kithara Playground")
                .font(.title2.bold())

            TextField("Audio URL", text: $model.url)
                .textFieldStyle(.roundedBorder)

            HStack {
                Button("Load") { model.load() }
                Button("Play") { model.play() }
                Button("Pause") { model.pause() }
            }

            VStack(alignment: .leading) {
                Text("Seek: \(Int(model.currentTime)) / \(Int(model.duration)) sec")
                Slider(value: $model.currentTime, in: 0...model.duration)
                Button("Seek") { model.seek() }
            }

            VStack(alignment: .leading) {
                Toggle("Mute", isOn: $model.isMuted)
                    .onChange(of: model.isMuted) { _, _ in model.setMuted() }
                Slider(value: Binding(
                    get: { Double(model.volume) },
                    set: { model.volume = Float($0) }
                ), in: 0...1)
                Button("Set volume") { model.commitVolume() }
            }

            HStack {
                Text("Rate")
                ForEach([0.5, 1.0, 1.5, 2.0], id: \.self) { value in
                    Button("\(value, specifier: "%.1fx")") { model.setRate(Float(value)) }
                }
            }

            Text("Status: \(model.status)")
            Text(model.log)
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
        .padding()
        .frame(width: 520)
    }
}

PlaygroundPage.current.setLiveView(PlaygroundView())
