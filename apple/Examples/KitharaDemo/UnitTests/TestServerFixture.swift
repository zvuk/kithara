import Foundation

enum TestServerFixture {
    static let environmentVariable = "KITHARA_TEST_SERVER_URL"

    static var baseURL: URL? {
        guard
            let value = ProcessInfo.processInfo.environment[environmentVariable],
            !value.isEmpty,
            let url = URL(string: value),
            let scheme = url.scheme,
            ["http", "https"].contains(scheme),
            url.host != nil
        else {
            return nil
        }
        return url
    }

    enum FixtureError: Error, LocalizedError {
        case missingBaseURL
        case invalidFixturePath(String)
        case invalidResponse(URL)
        case unexpectedStatus(URL, Int, String)

        var errorDescription: String? {
            switch self {
            case .missingBaseURL:
                "\(TestServerFixture.environmentVariable) is unset or is not a valid HTTP URL"
            case let .invalidFixturePath(path):
                "Could not build a test-server URL for fixture path \(path)"
            case let .invalidResponse(url):
                "Test server returned a non-HTTP response for \(url.absoluteString)"
            case let .unexpectedStatus(url, status, body):
                "Test server returned HTTP \(status) for \(url.absoluteString): \(body)"
            }
        }
    }

    enum Content: Encodable, Sendable {
        case htmlError
        case status(code: UInt16)
        case bytes(Data, contentType: String?)
        /// Serve a fixture the server already has on disk. Preferred over
        /// ``bytes(_:contentType:)`` for anything sizeable — uploading a
        /// multi-megabyte body is rejected by the request-body limit.
        case asset(name: String)

        private enum CodingKeys: String, CodingKey {
            case kind
            case code
            case base64
            case contentType = "content_type"
            case name
        }

        func encode(to encoder: Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case .htmlError:
                try container.encode("html_error", forKey: .kind)
            case let .status(code):
                try container.encode("status", forKey: .kind)
                try container.encode(code, forKey: .code)
            case let .bytes(data, contentType):
                try container.encode("bytes", forKey: .kind)
                try container.encode(data.base64EncodedString(), forKey: .base64)
                try container.encodeIfPresent(contentType, forKey: .contentType)
            case let .asset(name):
                try container.encode("asset", forKey: .kind)
                try container.encode(name, forKey: .name)
            }
        }
    }

    enum Delivery: Encodable, Sendable {
        case normal
        case range
        case earlyClose(afterBytes: Int)
        case stallAfter(afterBytes: Int)
        case throttle(chunk: Int, delayMilliseconds: UInt64)

        private enum CodingKeys: String, CodingKey {
            case kind
            case afterBytes = "after_bytes"
            case chunk
            case delayMilliseconds = "delay_ms"
        }

        func encode(to encoder: Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case .normal:
                try container.encode("normal", forKey: .kind)
            case .range:
                try container.encode("range", forKey: .kind)
            case let .earlyClose(afterBytes):
                try container.encode("early_close", forKey: .kind)
                try container.encode(afterBytes, forKey: .afterBytes)
            case let .stallAfter(afterBytes):
                try container.encode("stall_after", forKey: .kind)
                try container.encode(afterBytes, forKey: .afterBytes)
            case let .throttle(chunk, delayMilliseconds):
                try container.encode("throttle", forKey: .kind)
                try container.encode(chunk, forKey: .chunk)
                try container.encode(delayMilliseconds, forKey: .delayMilliseconds)
            }
        }
    }

    struct Behavior: Encodable, Sendable {
        let content: Content
        let delivery: Delivery
    }

    struct BehaviorHandle: Sendable {
        let token: String
        let url: URL

        func childURL(_ path: String) -> URL {
            path.split(separator: "/", omittingEmptySubsequences: true)
                .reduce(url) { result, component in
                    result.appendingPathComponent(String(component))
                }
        }
    }

    private struct NetworkRequest: Encodable {
        let online: Bool
    }

    private struct TokenResponse: Decodable {
        let token: String
    }

    static func requireBaseURL() throws -> URL {
        guard let baseURL else {
            throw FixtureError.missingBaseURL
        }
        return baseURL
    }

    static func url(_ path: String) throws -> URL {
        var baseURL = try requireBaseURL()
        baseURL.appendPathComponent("")
        let relativePath = path.drop(while: { $0 == "/" })
        guard let result = URL(string: String(relativePath), relativeTo: baseURL)?.absoluteURL else {
            throw FixtureError.invalidFixturePath(path)
        }
        return result
    }

    static func asset(_ name: String) throws -> URL {
        try url("assets/\(name.trimmingCharacters(in: CharacterSet(charactersIn: "/")))")
    }

    static func streamHQ(_ name: String) throws -> URL {
        var components = URLComponents(url: try url("streamhq"), resolvingAgainstBaseURL: false)
        let trimmedName = name.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        components?.queryItems = [URLQueryItem(name: "name", value: trimmedName)]
        guard let result = components?.url else {
            throw FixtureError.invalidFixturePath(name)
        }
        return result
    }

    static func setNetwork(online: Bool) async throws {
        _ = try await post(
            NetworkRequest(online: online),
            to: "control/network",
            expectedStatus: 204
        )
    }

    static func registerBehavior(_ behavior: Behavior) async throws -> BehaviorHandle {
        let data = try await post(
            behavior,
            to: "control/behavior",
            expectedStatus: 200
        )
        let token = try JSONDecoder().decode(TokenResponse.self, from: data).token
        return BehaviorHandle(token: token, url: try url("behavior/\(token)"))
    }

    private static func post<Body: Encodable>(
        _ body: Body,
        to path: String,
        expectedStatus: Int
    ) async throws -> Data {
        let endpoint = try url(path)
        var request = URLRequest(url: endpoint)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(body)

        let (data, response) = try await URLSession.shared.data(for: request)
        guard let response = response as? HTTPURLResponse else {
            throw FixtureError.invalidResponse(endpoint)
        }
        guard response.statusCode == expectedStatus else {
            let responseBody = String(data: data, encoding: .utf8) ?? "<non-UTF-8 body>"
            throw FixtureError.unexpectedStatus(endpoint, response.statusCode, responseBody)
        }
        return data
    }
}
