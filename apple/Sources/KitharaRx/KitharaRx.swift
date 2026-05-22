import Combine
import Foundation
import Kithara
import RxSwift

/// RxSwift bridge over Kithara's Combine publishers. Mirrors the
/// `rx*` accessor shape expected by consumers of the
/// `AudioPlayerProtocol` / `AudioPlayerItemProtocol` contracts.
public extension KitharaPlayer {
    /// Current queue item as a hot `Observable`. Emits `nil` when the
    /// queue is empty or the player is stopped. Rx mirror of
    /// `KitharaPlayer.currentItem`.
    var rxCurrentAudioItem: Observable<KitharaPlayerItem?> {
        currentItem.asObservable()
    }

    /// Playback rate (`0` paused, `1` normal, negative reverse).
    /// Rx mirror of `KitharaPlayer.rate`.
    var rxRate: Observable<Float> {
        rate.asObservable()
    }

    /// Live playback time in seconds. Drives sliders/progress bars
    /// without polling. Rx mirror of `KitharaPlayer.currentTimePublisher`.
    var rxCurrentTime: Observable<TimeInterval> {
        currentTimePublisher.asObservable()
    }

    /// Player-wide errors as a hot `Observable` of `PlayerError`.
    /// Rx mirror of `KitharaPlayer.error`.
    var rxError: Observable<Error> {
        error.asObservable()
    }
}

public extension KitharaPlayerItem {
    /// Buffered byte ranges for the underlying resource. Updates as
    /// the loader fetches new segments. Rx mirror of
    /// `KitharaPlayerItem.loadedRanges`.
    var rxLoadedRanges: Observable<[ItemLoadedRange]> {
        loadedRanges.asObservable()
    }

    /// Duration in seconds once the demuxer resolves it. Emits `nil`
    /// until then. Rx mirror of `KitharaPlayerItem.duration`.
    var rxDuration: Observable<Double?> {
        duration.asObservable()
    }

    /// Effective HLS variant bitrate in bits/sec for ABR-driven
    /// items. Rx mirror of `KitharaPlayerItem.bitrate`.
    var rxBitrate: Observable<Int32> {
        bitrate.asObservable()
    }

    /// Fires once when the item transitions to `readyToPlay`. Use
    /// this to chain UI affordances like enabling the play button.
    /// Rx mirror of `KitharaPlayerItem.readyToPlay`.
    var rxReadyToPlay: Observable<Void> {
        readyToPlay.asObservable()
    }

    /// Fires when the item finishes playing to the end of its
    /// timeline. Engine does not auto-advance — subscribe to drive
    /// queue progression. Rx mirror of `KitharaPlayerItem.didReachEnd`.
    var rxDidReachEnd: Observable<Void> {
        didReachEnd.asObservable()
    }

    /// Fires when playback stalls waiting on bytes. Used to surface
    /// rebuffering UI. Rx mirror of `KitharaPlayerItem.didStall`.
    var rxDidStall: Observable<Void> {
        didStall.asObservable()
    }

    /// Item-scoped error stream. Emits on transient demuxer/decoder
    /// failures and the fatal `didFail`. Rx mirror of
    /// `KitharaPlayerItem.error`.
    var rxError: Observable<Error> {
        error.asObservable()
    }
}

private extension AnyPublisher where Failure == Never {
    func asObservable() -> Observable<Output> {
        Observable.create { observer in
            let cancellable = self.sink { value in
                observer.onNext(value)
            }
            return Disposables.create { cancellable.cancel() }
        }
    }
}
