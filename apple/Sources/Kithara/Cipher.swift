import Foundation
import KitharaFFI

/// Position-dependent symmetric cipher for DRM key decryption.
///
/// Wraps the Rust `UniqueBinaryCipher` from `kithara-drm`. Conforms to
/// ``KeyProcessor`` so it can be passed directly into a
/// ``KitharaPlayer.KeyRule`` and registered through ``KitharaPlayer/init(config:)``.
///
/// The cipher closes over the secret supplied at construction time —
/// the per-call ``salt`` argument is ignored here. Use
/// ``KitharaPlayer/setupHlsAes(keyDecryptor:)`` with an inline closure
/// when the cipher needs to be derived from the salt on every decrypt.
public final class Cipher: KeyProcessor, @unchecked Sendable {
    private let _inner: FfiCipher

    /// Create a new cipher from a key string.
    public init(key: String) {
        self._inner = FfiCipher(key: key)
    }

    /// Decrypt data using this cipher.
    public func decrypt(_ data: Data) -> Data {
        Data(_inner.decrypt(data: data))
    }

    // MARK: - KeyProcessor

    public func processKey(_ key: Data, salt: String) -> Data {
        // Cipher's secret is fixed at init; salt is not consumed here.
        _ = salt
        return decrypt(key)
    }
}
