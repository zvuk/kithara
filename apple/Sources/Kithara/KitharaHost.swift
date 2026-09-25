import KitharaFFI

/// Process-wide Kithara audio host lifecycle.
public enum KitharaHost {
    /// Errors from explicit process-host initialization.
    public enum InitializationError: Error, Sendable {
        /// Another caller is currently initializing the host.
        case initializationInProgress
        /// The process host was already initialized.
        case alreadyInitialized
        /// Host settings were rejected or host construction failed.
        case failed(KitharaError)
    }

    /// Settings fixed for the lifetime of the process audio host.
    public struct Configuration: Sendable {
        /// Initial output sample-rate hint in hertz.
        public var sampleRateHint: UInt32
        /// Optional output callback size in frames.
        public var outputBlockFrames: UInt32?
        /// Output limiter settings validated when the host initializes.
        public var limiter: FfiLimiterConfig

        /// Create host settings from Rust defaults with optional overrides.
        public init(
            sampleRateHint: UInt32 = defaultHostConfig().sampleRateHint,
            outputBlockFrames: UInt32? = defaultHostConfig().outputBlockFrames,
            limiter: FfiLimiterConfig = defaultHostConfig().limiter
        ) {
            self.sampleRateHint = sampleRateHint
            self.outputBlockFrames = outputBlockFrames
            self.limiter = limiter
        }
    }

    /// Initialize the process audio host exactly once, before creating players.
    public static func initialize(configuration: Configuration = .init()) throws {
        do {
            try initializeHost(
                config: FfiHostConfig(
                    sampleRateHint: configuration.sampleRateHint,
                    outputBlockFrames: configuration.outputBlockFrames,
                    limiter: configuration.limiter
                )
            )
        } catch let error as FfiError {
            switch error {
            case .InitializationInProgress:
                throw InitializationError.initializationInProgress
            case .AlreadyInitialized:
                throw InitializationError.alreadyInitialized
            default:
                throw InitializationError.failed(KitharaError(ffi: error))
            }
        }
    }
}
