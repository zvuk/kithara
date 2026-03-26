import Foundation
import Kithara

/// Configure DRM key processing on the player.
///
/// Reads cipher key from `.env` file in the app bundle (key `DRM_KEY`),
/// falls back to `"kithara"` if not found.
/// Generates a random seed, creates a cipher, and sets up the key processor
/// with `X-Encrypted-Key` header.
func configureDrm(on player: KitharaPlayer) {
    let cipherKey = readEnvValue("DRM_KEY") ?? "kithara"
    let seed = randomAlphanumericSeed(length: 16)
    let secret = cipherKey + seed
    let cipher = Cipher(key: secret)

    player.setKeyProcessor(cipher, headers: ["X-Encrypted-Key": seed])
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

private func randomAlphanumericSeed(length: Int) -> String {
    let chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
    return String((0..<length).map { _ in chars.randomElement()! })
}
