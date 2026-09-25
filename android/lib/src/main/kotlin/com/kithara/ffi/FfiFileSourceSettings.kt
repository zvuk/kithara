package com.kithara.ffi

/** Creates File source settings with the previously available reader-event option. */
fun FfiFileSourceSettings(readerEventCapacity: UInt?): FfiFileSourceSettings =
    FfiFileSourceSettings(lookAheadBytes = null, readerEventCapacity = readerEventCapacity)
