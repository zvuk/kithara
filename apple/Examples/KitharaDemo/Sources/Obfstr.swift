import Foundation

/// Runtime de-obfuscation for build-time baked secrets.
///
/// The generator (`scripts/bake-secrets.swift`) XOR's each secret with
/// a per-build random pad of equal length and emits both byte arrays
/// into `Sources/Generated/Secrets.swift`. Neither array contains the
/// plain UTF-8 bytes, so `strings KitharaDemo` does not yield the
/// cipher key or auth token literals.
///
/// This is a `strings`-resistant obfuscation, not encryption — anyone
/// with the binary plus a debugger can still recover the secret. Use
/// it for internal demos, not for shipping production secrets.
enum Obfstr {
    static func reveal(_ obfuscated: [UInt8], _ pad: [UInt8]) -> String {
        precondition(obfuscated.count == pad.count, "obfuscated/pad length mismatch")
        let bytes = zip(obfuscated, pad).map { $0 ^ $1 }
        return String(decoding: bytes, as: UTF8.self)
    }
}
