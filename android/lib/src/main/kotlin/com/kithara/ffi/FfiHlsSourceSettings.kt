package com.kithara.ffi

/** Creates HLS source settings with the previously available batch-size option. */
fun FfiHlsSourceSettings(downloadBatchSize: UInt?): FfiHlsSourceSettings =
    FfiHlsSourceSettings(lookAheadBytes = null, sizeProbeMethod = null, acquireAttemptBudget = null, downloadBatchSize = downloadBatchSize)

/** Creates HLS source settings with the previously available probe and batch options. */
fun FfiHlsSourceSettings(sizeProbeMethod: FfiSizeProbeMethod?, downloadBatchSize: UInt?): FfiHlsSourceSettings =
    FfiHlsSourceSettings(lookAheadBytes = null, sizeProbeMethod = sizeProbeMethod, acquireAttemptBudget = null, downloadBatchSize = downloadBatchSize)

/** Creates HLS source settings with the previously available look-ahead, probe and batch options. */
fun FfiHlsSourceSettings(lookAheadBytes: ULong?, sizeProbeMethod: FfiSizeProbeMethod?, downloadBatchSize: UInt?): FfiHlsSourceSettings =
    FfiHlsSourceSettings(lookAheadBytes = lookAheadBytes, sizeProbeMethod = sizeProbeMethod, acquireAttemptBudget = null, downloadBatchSize = downloadBatchSize)
