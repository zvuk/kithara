import Combine
import Foundation
import Kithara
import RxSwift

final class KitharaZvukAudioPlayer: AudioPlayerProtocol {
    let kitharaPlayer: KitharaPlayer

    private let commandErrors = PublishSubject<PlayerError>()
    private let adapterLock = NSLock()
    private var itemAdapters: [Int64: KitharaZvukAudioPlayerItem] = [:]

    init(player: KitharaPlayer = KitharaPlayer()) {
        self.kitharaPlayer = player
    }

    var rxCurrentAudioItem: Observable<(any AudioPlayerItemProtocol)?> {
        kitharaPlayer.currentItem
            .asZvukObservable()
            .map { [weak self] item -> (any AudioPlayerItemProtocol)? in
                guard let self, let item else { return nil }
                return self.adapter(for: item)
            }
    }

    var rxRate: Observable<Float> {
        kitharaPlayer.rate.asZvukObservable()
    }

    var rxError: Observable<PlayerError> {
        let playbackErrors = kitharaPlayer.error
            .asZvukObservable()
            .compactMap { [weak self] error -> PlayerError? in
                guard let trackId = self?.currentAudioItem?.audioId else { return nil }
                return PlayerErrorMapper.map(error: error, trackId: trackId)
            }

        return Observable.merge(playbackErrors, commandErrors.asObservable())
    }

    var currentAudioItem: (any AudioPlayerItemProtocol)? {
        guard let item = kitharaPlayer.currentAudioItem else { return nil }
        return adapter(for: item)
    }

    var playingRate: Float {
        get { kitharaPlayer.playingRate }
        set { kitharaPlayer.playingRate = newValue }
    }

    var volume: Float {
        get { kitharaPlayer.volume }
        set { kitharaPlayer.volume = newValue }
    }

    var currentTime: Double {
        kitharaPlayer.currentTime
    }

    func items() -> [any AudioPlayerItemProtocol] {
        kitharaPlayer.items().map(adapter(for:))
    }

    func insert(_ item: any AudioPlayerItemProtocol, after afterItem: (any AudioPlayerItemProtocol)?) {
        guard let item = item as? KitharaZvukAudioPlayerItem else { return }
        let after = afterItem.flatMap { $0 as? KitharaZvukAudioPlayerItem }
        if afterItem != nil, after == nil { return }

        do {
            try kitharaPlayer.insert(item.kitharaItem, after: after?.kitharaItem)
            cache(item)
        } catch {
            commandErrors.onNext(PlayerErrorMapper.map(error: error, trackId: item.audioId))
        }
    }

    func remove(_ item: any AudioPlayerItemProtocol) {
        guard let item = item as? KitharaZvukAudioPlayerItem else { return }

        do {
            try kitharaPlayer.remove(item.kitharaItem)
            removeCachedAdapter(for: item)
        } catch {
            commandErrors.onNext(PlayerErrorMapper.map(error: error, trackId: item.audioId))
        }
    }

    func advanceToNextItem() {
        kitharaPlayer.advanceToNextItem()
    }

    func removeAllItems() {
        kitharaPlayer.removeAllItems()
        clearCachedAdapters()
    }

    func play() {
        kitharaPlayer.play()
    }

    func pause() {
        kitharaPlayer.pause()
    }

    func stop() {
        kitharaPlayer.stop()
        clearCachedAdapters()
    }

    func seek(to time: Double, tolerance: Double?, completionHandler: @escaping (Bool) -> Void) {
        kitharaPlayer.seek(
            to: time,
            tolerance: tolerance,
            completionHandler: KitharaZvukSeekCallback(completionHandler)
        )
    }

    func updatePeakBitrate(wifi: Double, cellular: Double) {
        kitharaPlayer.updatePeakBitrate(wifi: wifi, cellular: cellular)
    }

    func setupNetwork(authToken: String) {
        kitharaPlayer.setupNetwork(authToken: authToken)
    }

    func setupHlsAes(keyDecryptor: @escaping (_ key: Data, _ salt: String) -> Data?) {
        kitharaPlayer.setupHlsAes(keyDecryptor: keyDecryptor)
    }

    private func adapter(for item: KitharaPlayerItem) -> KitharaZvukAudioPlayerItem {
        adapterLock.lock()
        defer { adapterLock.unlock() }

        if let existing = itemAdapters[item.uuid] {
            return existing
        }

        let adapter = KitharaZvukAudioPlayerItem(kitharaItem: item)
        itemAdapters[item.uuid] = adapter
        return adapter
    }

    private func cache(_ adapter: KitharaZvukAudioPlayerItem) {
        adapterLock.lock()
        defer { adapterLock.unlock() }
        itemAdapters[adapter.uuid] = adapter
    }

    private func removeCachedAdapter(for adapter: KitharaZvukAudioPlayerItem) {
        adapterLock.lock()
        defer { adapterLock.unlock() }
        itemAdapters.removeValue(forKey: adapter.uuid)
    }

    private func clearCachedAdapters() {
        adapterLock.lock()
        defer { adapterLock.unlock() }
        itemAdapters.removeAll()
    }
}

final class KitharaZvukAudioPlayerItem: AudioPlayerItemProtocol {
    let kitharaItem: KitharaPlayerItem

    init(kitharaItem: KitharaPlayerItem) {
        self.kitharaItem = kitharaItem
    }

    convenience init(
        url: URL,
        audioId: TrackId,
        additionalHeaders: [String: String]? = nil,
        preferredPeakBitrate: Double = 0,
        preferredPeakBitrateForExpensiveNetworks: Double = 0,
        abrMode: AbrMode? = nil
    ) {
        self.init(
            kitharaItem: KitharaPlayerItem(
                url: url.absoluteString,
                audioId: audioId,
                additionalHeaders: additionalHeaders,
                preferredPeakBitrate: preferredPeakBitrate,
                preferredPeakBitrateForExpensiveNetworks: preferredPeakBitrateForExpensiveNetworks,
                abrMode: abrMode
            )
        )
    }

    var rxLoadedRanges: Observable<[AudioPlayer.ItemLoadedRange]> {
        kitharaItem.loadedRanges
            .asZvukObservable()
            .map { ranges in
                ranges.map { range in
                    AudioPlayer.ItemLoadedRange(start: range.start, duration: range.duration)
                }
            }
    }

    var rxReadyToPlay: Observable<Void> {
        kitharaItem.readyToPlay.asZvukObservable()
    }

    var rxDidReachEnd: Observable<Void> {
        kitharaItem.didReachEnd.asZvukObservable()
    }

    var rxDidStall: Observable<Void> {
        kitharaItem.didStall.asZvukObservable()
    }

    var rxBitrate: Observable<Int32> {
        kitharaItem.bitrate.asZvukObservable()
    }

    var durationSec: Double {
        kitharaItem.durationSec
    }

    var url: URL {
        kitharaItem.url
    }

    var audioId: TrackId {
        kitharaItem.audioId
    }

    var uuid: Int64 {
        kitharaItem.uuid
    }

    var isLiveStream: Bool {
        kitharaItem.isLiveStream
    }

    func load() -> Observable<AudioPlayer.ItemLoadResult> {
        Observable.just(
            AudioPlayer.ItemLoadResult(
                hasProtectedContent: false,
                isPlayable: true
            )
        )
    }

    func isPlayable(progress: Progress, and ranges: [AudioPlayer.ItemLoadedRange]) -> Bool {
        guard !isLiveStream else { return true }

        let time = progress.time
        let tolerance = 5.0
        return ranges.contains { range in
            let coversCurrentTime = time >= range.start - tolerance && time < range.start + range.duration
            return coversCurrentTime || progress.value >= 1
        }
    }
}

private final class KitharaZvukSeekCallback: SeekCallback, @unchecked Sendable {
    private let completionHandler: (Bool) -> Void

    init(_ completionHandler: @escaping (Bool) -> Void) {
        self.completionHandler = completionHandler
    }

    func onComplete(finished: Bool) {
        completionHandler(finished)
    }
}

private extension AnyPublisher where Failure == Never {
    func asZvukObservable() -> Observable<Output> {
        Observable.create { observer in
            let cancellable = self.sink { value in
                observer.onNext(value)
            }
            return Disposables.create {
                cancellable.cancel()
            }
        }
    }
}
