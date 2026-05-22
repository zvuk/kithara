import Combine
import Foundation
import Kithara
import RxSwift

/// RxSwift bridge over Kithara's Combine publishers. Mirrors the
/// `rx*` accessor shape that `TvoyZvuk` consumes from its existing
/// `AudioPlayerProtocol` / `AudioPlayerItemProtocol` contracts.
public extension KitharaPlayer {
    var rxCurrentAudioItem: Observable<KitharaPlayerItem?> {
        currentItem.asObservable()
    }

    var rxRate: Observable<Float> {
        rate.asObservable()
    }

    var rxError: Observable<Error> {
        error.asObservable()
    }
}

public extension KitharaPlayerItem {
    var rxLoadedRanges: Observable<[ItemLoadedRange]> {
        loadedRanges.asObservable()
    }

    var rxDuration: Observable<Double?> {
        duration.asObservable()
    }

    var rxBitrate: Observable<Int32> {
        bitrate.asObservable()
    }

    var rxReadyToPlay: Observable<Void> {
        readyToPlay.asObservable()
    }

    var rxDidReachEnd: Observable<Void> {
        didReachEnd.asObservable()
    }

    var rxDidStall: Observable<Void> {
        didStall.asObservable()
    }

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
