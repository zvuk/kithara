import Combine
import Foundation
import Kithara
import RxSwift

final class KitharaZvukAudioPlayer: AudioPlayerProtocol {
    let kitharaPlayer: KitharaPlayer

    init(player: KitharaPlayer = KitharaPlayer()) {
        Kithara.initLogging(level: .debug)
        self.kitharaPlayer = player
    }

    var rxCurrentAudioItem: Observable<(any AudioPlayerItemProtocol)?> {
        kitharaPlayer.currentItemRepresentationPublisher(as: KitharaZvukAudioPlayerItem.self)
            .asRxObservable()
            .map { item -> (any AudioPlayerItemProtocol)? in
                item
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
        kitharaPlayer.currentItemRepresentation(as: KitharaZvukAudioPlayerItem.self)
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
        kitharaPlayer.itemRepresentations(as: KitharaZvukAudioPlayerItem.self)
    }

    func insert(_ item: any AudioPlayerItemProtocol, after afterItem: (any AudioPlayerItemProtocol)?) {
        guard let item = item as? KitharaZvukAudioPlayerItem else { return }
        let after = afterItem.flatMap { $0 as? KitharaZvukAudioPlayerItem }
        guard afterItem == nil || after != nil else { return }

        try? kitharaPlayer.insert(
            item.kitharaPlayerItem,
            representing: item,
            after: after?.kitharaPlayerItem
        )
    }

    func remove(_ item: any AudioPlayerItemProtocol) {
        guard let item = item as? KitharaZvukAudioPlayerItem else { return }

        try? kitharaPlayer.remove(item.kitharaPlayerItem)
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

final class KitharaZvukAudioPlayerItem: AudioPlayerItemProtocol, @unchecked Sendable {
    fileprivate let kitharaPlayerItem: KitharaPlayerItem
    private let playabilityStartTolerance: TimeInterval

    init(
        url: URL,
        audioId: TrackId? = nil,
        uuid: Int64? = nil,
        playabilityStartTolerance: TimeInterval = .zero,
        additionalHeaders: [String: String]? = nil,
        preferredPeakBitrate: Double = 0,
        preferredPeakBitrateForExpensiveNetworks: Double = 0,
        abrMode: AbrMode? = nil
    ) {
        self.playabilityStartTolerance = playabilityStartTolerance
        self.kitharaPlayerItem = KitharaPlayerItem(
            url: url.absoluteString,
            audioId: audioId,
            uuid: uuid,
            additionalHeaders: additionalHeaders,
            preferredPeakBitrate: preferredPeakBitrate,
            preferredPeakBitrateForExpensiveNetworks: preferredPeakBitrateForExpensiveNetworks,
            abrMode: abrMode
        )
    }

    var durationSec: Double {
        kitharaPlayerItem.durationSec
    }

    var url: URL {
        kitharaPlayerItem.url
    }

    var audioId: TrackId {
        kitharaPlayerItem.audioId
    }

    var uuid: Int64 {
        kitharaPlayerItem.uuid
    }

    var isLiveStream: Bool {
        kitharaPlayerItem.isLiveStream
    }

    var rxLoadedRanges: Observable<[AudioPlayer.ItemLoadedRange]> {
        kitharaPlayerItem.loadedRanges
            .asRxObservable()
            .map { ranges in
                ranges.map { range in
                    AudioPlayer.ItemLoadedRange(start: range.start, duration: range.duration)
                }
            }
    }

    var rxReadyToPlay: Observable<Void> {
        kitharaPlayerItem.readyToPlay.asRxObservable()
    }

    var rxDidReachEnd: Observable<Void> {
        kitharaPlayerItem.didReachEnd.asRxObservable()
    }

    var rxDidStall: Observable<Void> {
        kitharaPlayerItem.didStall.asRxObservable()
    }

    var rxBitrate: Observable<Int32> {
        kitharaPlayerItem.bitrate.asRxObservable()
    }

    func load() -> Observable<AudioPlayer.ItemLoadResult> {
        Observable.create { [kitharaPlayerItem] observer in
            let task = Task {
                let result = await kitharaPlayerItem.load()
                observer.onNext(
                    AudioPlayer.ItemLoadResult(
                        hasProtectedContent: result.hasProtectedContent,
                        isPlayable: result.isPlayable
                    )
                )
                observer.onCompleted()
            }
            return Disposables.create { task.cancel() }
        }
    }

    func isPlayable(progress: Progress, and ranges: [AudioPlayer.ItemLoadedRange]) -> Bool {
        kitharaPlayerItem.isPlayable(
            progress: progress,
            ranges: ranges,
            startTolerance: playabilityStartTolerance
        )
    }
}

extension Player.Progress: PlaybackProgressSource {}
extension AudioPlayer.ItemLoadedRange: ItemLoadedRangeSource {}

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
