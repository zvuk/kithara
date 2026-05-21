import Foundation
import Kithara

// MARK: - .env reader

/// Read a value from the `.env` file bundled in the app resources.
/// The same workspace `.env` powers `kithara-app`'s `build.rs` baking
/// — both surfaces consume the same environment variables.
func readEnvValue(_ key: String) -> String? {
    guard let path = Bundle.main.path(forResource: ".env", ofType: nil)
            ?? Bundle.main.path(forResource: "env", ofType: nil),
          let contents = try? String(contentsOfFile: path, encoding: .utf8)
    else {
        return nil
    }

    for line in contents.components(separatedBy: .newlines) {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty || trimmed.hasPrefix("#") { continue }
        let parts = trimmed.split(separator: "=", maxSplits: 1)
        if parts.count == 2, String(parts[0]).trimmingCharacters(in: .whitespaces) == key {
            return String(parts[1]).trimmingCharacters(in: .whitespaces)
        }
    }
    return nil
}

// MARK: - DRM provider config (mirrors `crates/kithara-app/app.yaml`)

/// Per-provider DRM configuration, mirrored from
/// `crates/kithara-app/app.yaml`'s `drm.providers` list. Each
/// provider has its own cipher key, X-Encrypted-Key salt format,
/// HTTP headers, and domain matcher.
///
/// Without registering BOTH providers here, prod (zvuk.com) and stage
/// (zvq.me) tracks fail with HTTP 418 (WAF rejecting wrong-format
/// salt) or HTTP 401 (missing X-Auth-Token) and the player surfaces
/// the failure long before any seek-cascade bug can be reproduced.
struct DrmProvider {
    /// Display name (matches `app.yaml`'s `name:` field).
    let name: String
    /// Domain patterns: `zvuk.com` (exact) or `*.zvuk.com` (subdomain
    /// wildcard). Must list both forms when the key URL lives on the
    /// bare host but media is on subdomains.
    let domains: [String]
    /// Bytes of the cipher master key. The runtime decryptor builds
    /// the working cipher from `cipherKey + salt` on every decrypt.
    let cipherKey: String
    /// Seed alphabet for `X-Encrypted-Key` salt generation.
    let seedAlphabet: SeedAlphabet
    /// Salt length in characters.
    let seedLength: Int
    /// Static HTTP headers attached to every request matched by this
    /// provider (playlist, segment, key URL).
    let headers: [String: String]

    enum SeedAlphabet {
        /// 0-9 a-f. Matches the iOS production
        /// `HLSAes128Service.AES128ResourceLoader.randomString(of: 8)`.
        case hex
        /// 0-9 a-z A-Z. Legacy zvqengine format used by stage.
        case alphanumeric
    }
}

/// Build the prod + stage providers from the bundled `.env`. If a
/// required env var is missing the provider is omitted — the player
/// will surface a key-fetch failure when an item from that domain
/// loads, which is the same behaviour `kithara-app` shows.
func bundledDrmProviders() -> [DrmProvider] {
    var providers: [DrmProvider] = []

    if let prodKey = readEnvValue("KITHARA_DRM_PROD_KEY") {
        var headers: [String: String] = [
            "User-Agent": "OpenPlay - com.zvooq.openplay/4.30.0 (iPhone; iOS 17.5; Scale/3.00)"
        ]
        if let auth = readEnvValue("KITHARA_DRM_PROD_AUTH_TOKEN") {
            headers["X-Auth-Token"] = auth
        }
        if let spZv = readEnvValue("KITHARA_DRM_PROD_SP_ZV_TOKEN") {
            headers["X-SP-ZV"] = spZv
        }
        providers.append(
            DrmProvider(
                name: "zvuk-prod",
                domains: ["zvuk.com", "*.zvuk.com"],
                cipherKey: prodKey,
                seedAlphabet: .hex,
                seedLength: 8,
                headers: headers
            )
        )
    }

    if let stageKey = readEnvValue("KITHARA_DRM_STAGE_KEY") {
        var headers: [String: String] = [
            "User-Agent": "OpenPlay - com.zvooq.openplay/4.30.0 (iPhone; iOS 17.5; Scale/3.00)"
        ]
        if let auth = readEnvValue("KITHARA_DRM_STAGE_AUTH_TOKEN") {
            headers["X-Auth-Token"] = auth
        }
        providers.append(
            DrmProvider(
                name: "zvuk-stage",
                domains: ["zvq.me", "*.zvq.me"],
                cipherKey: stageKey,
                seedAlphabet: .alphanumeric,
                seedLength: 16,
                headers: headers
            )
        )
    }

    return providers
}

// MARK: - Salt generation (matches per-provider seed spec)

func generateSalt(alphabet: DrmProvider.SeedAlphabet, length: Int) -> String {
    let chars: [Character] = {
        switch alphabet {
        case .hex:
            return Array("0123456789abcdef")
        case .alphanumeric:
            return Array("0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ")
        }
    }()
    var out = ""
    out.reserveCapacity(length)
    for _ in 0..<length {
        out.append(chars[Int.random(in: 0..<chars.count)])
    }
    return out
}

// MARK: - KeyProcessor closure adapter

/// Adapts a `(Data, String) -> Data` closure into a `KeyProcessor`
/// so that per-provider decryptors built from the bundled cipher key
/// can be registered through `KitharaPlayer.setupHlsAes(rule:)`.
final class ClosureKeyProcessor: KeyProcessor, @unchecked Sendable {
    private let decrypt: (Data, String) -> Data

    init(decrypt: @escaping (Data, String) -> Data) {
        self.decrypt = decrypt
    }

    func processKey(_ key: Data, salt: String) -> Data {
        decrypt(key, salt)
    }
}
