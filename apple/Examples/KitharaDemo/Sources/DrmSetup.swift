import Foundation
import Kithara

/// Read the cipher key for zvuk DRM from the bundled `.env` file.
///
/// The salt is supplied per-call by the player (see
/// `KitharaPlayer.setupHlsAes`), so the demo no longer pre-generates a
/// seed — the closure builds the cipher on every decrypt from
/// `cipherKey + salt` to match the server's encryption.
func readZvukCipherKey() -> String {
    readEnvValue("DRM_KEY") ?? "BinaryCipherKey"
}

/// Auth token for zvuk-style protected streams. Mirrors the
/// `KITHARA_DRM_AUTH_TOKEN` baked at compile-time in `kithara-app`.
func readZvukAuthToken() -> String? {
    readEnvValue("KITHARA_DRM_AUTH_TOKEN")
}

/// Read a value from `.env` file bundled in the app.
private func readEnvValue(_ key: String) -> String? {
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
