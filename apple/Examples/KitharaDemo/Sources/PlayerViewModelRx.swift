#if KITHARA_RX
import Foundation
import Kithara
import KitharaRx
import RxSwift

/// RxSwift variant of the demo view-model. Subscribes to the
/// per-event `Observable<…>` streams exposed by `KitharaRx`
/// (`rxRate`, `rxCurrentAudioItem`, `rxCurrentTime`, `rxError` at
/// the player level; `rxDuration`, `rxBitrate`, `rxDidReachEnd`,
/// `rxDidStall`, `rxError` per item). No `import Combine` lives in
/// this file — `@Published` storage comes from `ObservableObject`
/// in the base class.
@MainActor
final class PlayerViewModelRx: PlayerViewModelBase {
    /// Player-level subscriptions.
    private let bag = DisposeBag()
    /// Per-item Rx subscription bags, keyed by `audioId`. Removing
    /// an item drops the bag → disposes every subscription it owns.
    private var itemBags: [TrackId: DisposeBag] = [:]

    override func bindEvents() {
        player.rxCurrentAudioItem
            .observe(on: MainScheduler.instance)
            .subscribe(onNext: { [weak self] item in
                guard let self else { return }
                let id = item?.audioId
                self.currentTrackId = id
                self.resetPerTrackUi(trackId: id)
                if let item {
                    self.subscribeItem(item)
                }
            })
            .disposed(by: bag)

        player.rxRate
            .observe(on: MainScheduler.instance)
            .subscribe(onNext: { [weak self] rate in
                self?.isPlaying = rate > 0
            })
            .disposed(by: bag)

        player.rxCurrentTime
            .observe(on: MainScheduler.instance)
            .subscribe(onNext: { [weak self] seconds in
                guard let self, !self.isSeeking else { return }
                self.currentTime = seconds
            })
            .disposed(by: bag)

        player.rxError
            .observe(on: MainScheduler.instance)
            .subscribe(onNext: { [weak self] err in
                guard let self else { return }
                self.errorMessage = "\(err)"
                self.status = .failed
            })
            .disposed(by: bag)
    }

    override func subscribeItem(_ item: KitharaPlayerItem) {
        let entryId = item.audioId
        guard itemBags[entryId] == nil else { return }
        let itemBag = DisposeBag()

        item.rxDuration
            .observe(on: MainScheduler.instance)
            .subscribe(onNext: { [weak self] seconds in
                guard let self else { return }
                if let idx = self.playlist.firstIndex(where: { $0.id == entryId }) {
                    self.playlist[idx].duration = seconds
                }
                if entryId == self.currentTrackId {
                    self.duration = seconds
                }
            })
            .disposed(by: itemBag)

        item.rxBitrate
            .observe(on: MainScheduler.instance)
            .subscribe(onNext: { [weak self] bps in
                guard let self, entryId == self.currentTrackId else { return }
                self.currentVariantLabel = "\(bps / 1000) kbps"
            })
            .disposed(by: itemBag)

        item.rxError
            .observe(on: MainScheduler.instance)
            .subscribe(onNext: { [weak self] err in
                guard let self, entryId == self.currentTrackId else { return }
                if self.errorMessage == nil {
                    self.errorMessage = "\(err)"
                }
            })
            .disposed(by: itemBag)

        item.rxDidReachEnd
            .observe(on: MainScheduler.instance)
            .subscribe(onNext: { [weak self] _ in
                guard let self else { return }
                // Engine does not auto-advance between queue items;
                // drive the queue forward explicitly. Mirrors
                // AVQueuePlayer.
                self.player.advanceToNextItem()
            })
            .disposed(by: itemBag)

        item.rxDidStall
            .observe(on: MainScheduler.instance)
            .subscribe(onNext: { _ in
                print("[KitharaDemo] item \(entryId) stalled")
            })
            .disposed(by: itemBag)

        itemBags[entryId] = itemBag
    }

    override func unsubscribeItem(_ trackId: TrackId) {
        itemBags.removeValue(forKey: trackId)
    }
}
#endif
