import Foundation
import KitharaFFI

/// Position-dependent symmetric cipher for DRM key decryption.
///
/// Wraps the Rust `UniqueBinaryCipher` from `kithara-drm`.
/// Conforms to ``KeyProcessor`` so it can be passed directly
/// to ``KitharaPlayer/setKeyProcessor(_:headers:)``.
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

    public func processKey(_ key: Data) -> Data {
        decrypt(key)
    }
}
