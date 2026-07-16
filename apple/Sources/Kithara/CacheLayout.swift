import KitharaFFI

/// Asset identity passed to ``CacheLayoutDelegate/root(source:)``.
public typealias CacheAssetSource = FfiAssetSource

/// Resource identity passed to ``CacheLayoutDelegate/path(resource:)``.
public typealias CacheAssetResource = FfiAssetResource

/// Maps asset sources and resources to paths inside Kithara's cache.
///
/// Implementations must be `Sendable`, pure, deterministic, fast,
/// non-blocking, and non-throwing. Callbacks may run on background threads.
/// Returned paths must not contain query parameters, credentials, or secrets.
/// `root` returns exactly one non-empty component and cannot equal `_index`.
/// `path` returns a non-empty relative path separated by `/`; no component may
/// end in `.tmp`. Components are ASCII, at most 96 bytes, never `.` or `..`,
/// do not end in a dot or space, are not Windows device names, and contain
/// neither control bytes nor `< > : " / \ | ? *`. These reserved-name checks
/// are case-insensitive. Invalid output is rejected rather than rewritten.
public typealias CacheLayoutDelegate = FfiAssetLayout

/// Playback source whose default cache layout can be replaced.
public enum CacheLayoutTarget: CaseIterable, Hashable, Sendable {
    /// Direct file playback.
    case file
    /// HTTP Live Streaming playback.
    case hls

    fileprivate var ffiValue: FfiCacheLayoutTarget {
        switch self {
        case .file:
            .file
        case .hls:
            .hls
        }
    }
}

/// Cache layouts registered before player creation.
///
/// A target has at most one layout. Registering another layout for the same
/// target replaces the previous registration.
public struct CacheLayoutRegistry: Sendable {
    private var layouts: [CacheLayoutTarget: CacheLayoutDelegate] = [:]

    /// Creates an empty registry that keeps Kithara's default layouts.
    public init() {}

    /// Registers `layout` for `target`, replacing an existing registration.
    public mutating func register(
        _ layout: CacheLayoutDelegate,
        for target: CacheLayoutTarget
    ) {
        layouts[target] = layout
    }

    var ffiRegistrations: [FfiCacheLayoutRegistration] {
        CacheLayoutTarget.allCases.compactMap { target in
            layouts[target].map {
                FfiCacheLayoutRegistration(target: target.ffiValue, layout: $0)
            }
        }
    }
}
