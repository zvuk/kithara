import Combine
import Foundation
import Kithara
import RxSwift

final class KitharaZvukAudioPlayer: AudioPlayerProtocol {
    let kitharaPlayer: KitharaPlayer

    init(player: KitharaPlayer = KitharaPlayer()) {
        Kithara.initLogging(level: .info)
        self.kitharaPlayer = player
    }

    var rxCurrentAudioItem: Observable<(any AudioPlayerItemProtocol)?> {
        kitharaPlayer.currentItem
            .asRxObservable()
            .map { item -> (any AudioPlayerItemProtocol)? in
                item as? KitharaZvukAudioPlayerItem
            }
    }

    var rxRate: Observable<Float> {
        kitharaPlayer.rate.asRxObservable()
    }

    var rxError: Observable<PlayerError> {
        kitharaPlayer.contextualError
            .asRxObservable()
            .compactMap { PlayerErrorMapper.map(error: $0) }
    }

    var currentAudioItem: (any AudioPlayerItemProtocol)? {
        kitharaPlayer.currentAudioItem as? KitharaZvukAudioPlayerItem
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
        kitharaPlayer.items().compactMap { $0 as? KitharaZvukAudioPlayerItem }
    }

    func insert(_ item: any AudioPlayerItemProtocol, after afterItem: (any AudioPlayerItemProtocol)?) {
        guard let item = item.kitharaPlayerItem else { return }
        let after = afterItem?.kitharaPlayerItem
        guard afterItem == nil || after != nil else { return }

        try? kitharaPlayer.insert(item, after: after)
    }

    func remove(_ item: any AudioPlayerItemProtocol) {
        guard let item = item.kitharaPlayerItem else { return }

        try? kitharaPlayer.remove(item)
    }

    func advanceToNextItem() {
        kitharaPlayer.advanceToNextItem()
    }

    func removeAllItems() {
        kitharaPlayer.removeAllItems()
    }

    func play() {
        kitharaPlayer.play()
    }

    func pause() {
        kitharaPlayer.pause()
    }

    func stop() {
        kitharaPlayer.stop()
    }

    func seek(to time: Double, tolerance: Double?, completionHandler: @escaping (Bool) -> Void) {
        kitharaPlayer.seek(
            to: time,
            tolerance: tolerance,
            completionHandler: completionHandler
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
}

final class KitharaZvukAudioPlayerItem: KitharaPlayerItem, AudioPlayerItemProtocol, @unchecked Sendable {
    private let playabilityStartTolerance: TimeInterval

    init(
        url: URL,
        audioId: TrackId,
        uuid: Int64,
        playabilityStartTolerance: TimeInterval = .zero,
        additionalHeaders: [String: String]? = nil,
        preferredPeakBitrate: Double = 0,
        preferredPeakBitrateForExpensiveNetworks: Double = 0,
        abrMode: AbrMode? = nil
    ) {
        self.playabilityStartTolerance = playabilityStartTolerance
        super.init(
            url: url.absoluteString,
            audioId: audioId,
            uuid: uuid,
            additionalHeaders: additionalHeaders,
            preferredPeakBitrate: preferredPeakBitrate,
            preferredPeakBitrateForExpensiveNetworks: preferredPeakBitrateForExpensiveNetworks,
            abrMode: abrMode
        )
    }

    var rxLoadedRanges: Observable<[AudioPlayer.ItemLoadedRange]> {
        loadedRanges
            .asRxObservable()
            .map { ranges in
                ranges.map { range in
                    AudioPlayer.ItemLoadedRange(start: range.start, duration: range.duration)
                }
            }
    }

    var rxReadyToPlay: Observable<Void> {
        readyToPlay.asRxObservable()
    }

    var rxDidReachEnd: Observable<Void> {
        didReachEnd.asRxObservable()
    }

    var rxDidStall: Observable<Void> {
        didStall.asRxObservable()
    }

    var rxBitrate: Observable<Int32> {
        bitrate.asRxObservable()
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
        isPlayable(progress: progress, ranges: ranges, startTolerance: playabilityStartTolerance)
    }
}

extension Player.Progress: PlaybackProgressSource {}
extension AudioPlayer.ItemLoadedRange: ItemLoadedRangeSource {}

private extension AudioPlayerItemProtocol {
    var kitharaPlayerItem: KitharaPlayerItem? {
        self as? KitharaPlayerItem
    }
}

private extension PlayerErrorMapper {
    static func map(error: KitharaPlayerError) -> PlayerError? {
        guard let trackId = error.itemId else { return nil }
        return map(error: error.underlying, trackId: trackId)
    }
}

private extension Publisher where Failure == Never {
    func asRxObservable() -> Observable<Output> {
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
