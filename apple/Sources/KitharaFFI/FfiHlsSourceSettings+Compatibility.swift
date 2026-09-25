extension FfiHlsSourceSettings {
    /// Creates HLS source settings with the previously available batch-size option.
    public init(downloadBatchSize: UInt32?) {
        self.init(lookAheadBytes: nil, sizeProbeMethod: nil, acquireAttemptBudget: nil, downloadBatchSize: downloadBatchSize)
    }

    /// Creates HLS source settings with the previously available probe and batch options.
    public init(sizeProbeMethod: FfiSizeProbeMethod?, downloadBatchSize: UInt32?) {
        self.init(lookAheadBytes: nil, sizeProbeMethod: sizeProbeMethod, acquireAttemptBudget: nil, downloadBatchSize: downloadBatchSize)
    }

    /// Creates HLS source settings with the previously available look-ahead, probe and batch options.
    public init(lookAheadBytes: UInt64?, sizeProbeMethod: FfiSizeProbeMethod?, downloadBatchSize: UInt32?) {
        self.init(lookAheadBytes: lookAheadBytes, sizeProbeMethod: sizeProbeMethod, acquireAttemptBudget: nil, downloadBatchSize: downloadBatchSize)
    }
}
