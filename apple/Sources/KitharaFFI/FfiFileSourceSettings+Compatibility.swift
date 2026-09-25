extension FfiFileSourceSettings {
    /// Creates File source settings with the previously available reader-event option.
    public init(readerEventCapacity: UInt32?) {
        self.init(lookAheadBytes: nil, readerEventCapacity: readerEventCapacity)
    }
}
