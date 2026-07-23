import KitharaFFI

/// Asset identity passed to ``AssetLayout/root(source:)``.
public typealias AssetSource = FfiAssetSource

/// Resource identity passed to ``AssetLayout/path(resource:)``.
/// `.source` is direct-file media, `.url` covers HLS playlists, init segments,
/// media segments, and keys, and `.named` covers analysis and other derived
/// artifacts.
public typealias AssetResource = FfiAssetResource

/// Maps asset sources and resources to paths inside Kithara's cache.
///
/// Implementations must be `Sendable`, pure, deterministic, fast,
/// non-blocking, and non-throwing. Callbacks may run on background threads.
/// Returned paths must not contain query parameters, credentials, or secrets.
/// `root` is called once whenever a store scope is created. `path` is called
/// once whenever a resource key is minted. Cache reads and writes using that
/// key do not invoke either callback again.
/// A `.url` resource contains the full URL. Custom layouts must preserve any
/// required query identity without returning raw query text; Kithara's default
/// layout uses a bounded query fingerprint and ignores fragments.
/// `root` returns exactly one non-empty component and cannot equal `_index`.
/// `path` returns a non-empty relative path separated by `/`; no component may
/// end in `.tmp`. Components are ASCII, at most 96 bytes, never `.` or `..`,
/// do not end in a dot or space, are not Windows device names, and contain
/// neither control bytes nor `< > : " / \ | ? *`. These reserved-name checks
/// are case-insensitive. Invalid output fails scope or key creation rather than
/// being rewritten or replaced with the default layout.
public typealias AssetLayout = FfiAssetLayout

/// Playback source whose default asset layout can be replaced.
public enum AssetLayoutTarget: Sendable {
    /// Direct-file playback.
    case file
    /// HTTP Live Streaming playback.
    case hls

    fileprivate var ffiValue: FfiAssetLayoutTarget {
        switch self {
        case .file:
            .file
        case .hls:
            .hls
        }
    }
}

/// Rust-owned registry of protocol-specific asset layouts.
///
/// A target has at most one layout. Registration is routed to Rust
/// immediately, and registering another layout for the same target replaces
/// the previous registration.
public final class AssetLayoutRegistry: @unchecked Sendable {
    let inner: FfiAssetLayoutRegistry

    /// Creates an empty registry that keeps Kithara's default layouts.
    public init() {
        self.inner = FfiAssetLayoutRegistry()
    }

    /// Registers `layout` for `target`, replacing an existing registration.
    public func register(_ layout: AssetLayout, for target: AssetLayoutTarget) {
        inner.register(target: target.ffiValue, layout: layout)
    }
}

/// Rust-owned asset store that can be shared by multiple players.
///
/// The store captures a snapshot of `layouts` during initialization. Later
/// registry changes apply only to stores created afterward.
public final class AssetStore: @unchecked Sendable {
    let inner: FfiAssetStore

    /// Creates an asset store rooted at `root` with a snapshot of `layouts`.
    /// `nil` uses Kithara's platform-default cache directory.
    public init(root: String? = nil, layouts: AssetLayoutRegistry = .init()) {
        self.inner = FfiAssetStore(root: root, layouts: layouts.inner)
    }
}
