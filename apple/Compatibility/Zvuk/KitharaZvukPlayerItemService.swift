import Foundation
import Core
import Kithara
import RxSwift

final class KitharaZvukPlayerItemService: PlayerItemService {
    enum Error: Swift.Error {
        case legacyFairplayDisabled
        case invalidHeaderName
        case invalidHeaderValue(name: String)
    }

    var fairplayHlsEnabled = false
    private let playabilityStartTolerance: TimeInterval

    init(playabilityStartTolerance: TimeInterval = 5.0) {
        self.playabilityStartTolerance = playabilityStartTolerance
    }

    func makePlayerItem(
        track: StreamToPlay,
        streamHeaders: [AnyHashable: Any]?
    ) -> Observable<Event<any AudioPlayerItemProtocol>> {
        if track.streamType == .fairplay && !fairplayHlsEnabled {
            return .just(.error(Error.legacyFairplayDisabled))
        }

        let headers: [String: String]?
        do {
            headers = try Self.headers(from: streamHeaders)
        } catch {
            return .just(.error(error))
        }

        let item = KitharaZvukAudioPlayerItem(
            url: track.url,
            audioId: track.id,
            uuid: track.uuid,
            playabilityStartTolerance: playabilityStartTolerance,
            additionalHeaders: headers
        )

        // Match the host AVPlayer item lifecycle: start load() inside
        // makePlayerItem and surface the item once load resolves. For the
        // Kithara backend the heavy loading is driven by insert(); load()
        // resolves the current readiness snapshot.
        return item.load()
            .mapTo(item as any AudioPlayerItemProtocol)
            .materialize()
    }

    private static func headers(from raw: [AnyHashable: Any]?) throws -> [String: String]? {
        guard let raw else { return nil }

        var headers: [String: String] = [:]
        for (key, value) in raw {
            guard let name = key.base as? String else {
                throw Error.invalidHeaderName
            }
            guard let value = value as? String else {
                throw Error.invalidHeaderValue(name: name)
            }
            headers[name] = value
        }

        return headers.isEmpty ? nil : headers
    }
}
