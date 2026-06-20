import Foundation
import Kithara
import RxSwift

final class KitharaZvukPlayerItemService: PlayerItemService {
    enum Error: Swift.Error {
        case unsupportedLegacyFairplay
        case invalidHeaderName
        case invalidHeaderValue(name: String)
    }

    var fairplayHlsEnabled = false

    func makePlayerItem(
        track: StreamToPlay,
        streamHeaders: [AnyHashable: Any]?
    ) -> Observable<Event<any AudioPlayerItemProtocol>> {
        guard track.streamType != .fairplay else {
            return .just(.error(Error.unsupportedLegacyFairplay))
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
            additionalHeaders: headers
        )

        return .just(.next(item))
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
